//! Newly authored neutral fixtures through the actual registry, QuickJS and HTML host.
use reader_sources::{
    CancelToken, HttpRequest, HttpResponse, Source, SourceError, SourceRegistry, SourceTransport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    path::Path,
    sync::{Arc, Mutex},
};
const ID: &str = "mitaku";
const BASE: &str = "https://mitaku.net";
const GALLERY: &str = "/gallery/clockwork";
const HTML_CATALOGUE: &str = "popular";
fn addon_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/addons")
        .join(ID)
}
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(addon_root().join("tests/fixtures").join(name)).unwrap()
}
fn archive() -> Vec<u8> {
    let root = addon_root();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut names = vec![
        "manifest.json".to_owned(),
        manifest["entry"].as_str().unwrap().to_owned(),
    ];
    for key in ["scripts", "assets"] {
        names.extend(
            manifest[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned()),
        );
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in names {
        zip.start_file(
            &name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&std::fs::read(root.join(name)).unwrap())
            .unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[derive(Default)]
struct FixtureTransport {
    requests: Mutex<Vec<HttpRequest>>,
    override_response: Mutex<Option<(u16, Vec<u8>)>>,
    redirect_search: Mutex<bool>,
}
impl SourceTransport for FixtureTransport {
    fn request(
        &self,
        request: &HttpRequest,
        _: usize,
        cancel: &CancelToken,
    ) -> reader_sources::Result<HttpResponse> {
        cancel.check()?;
        self.requests.lock().unwrap().push(request.clone());
        let url = url::Url::parse(&request.url).unwrap();
        assert_eq!(
            request.headers.get("Origin").map(String::as_str),
            Some(BASE)
        );
        assert!(request.headers.get("Referer").unwrap().starts_with(BASE));
        if url.path().ends_with(".svg") {
            return Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                headers: BTreeMap::new(),
                body: b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>"
                    .to_vec(),
            });
        }
        let redirect =
            *self.redirect_search.lock().unwrap() && url.query_pairs().any(|(key, _)| key == "s");
        let (status, body) = self
            .override_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                (
                    200,
                    fixture(if redirect || url.path().trim_end_matches('/') == GALLERY {
                        "details.html"
                    } else if url.path().starts_with("/wp-json/") {
                        "popular.json"
                    } else if url.path() == "/explore-categories/" {
                        "categories.html"
                    } else {
                        "list.html"
                    }),
                )
            });
        Ok(HttpResponse {
            status,
            url: if redirect {
                format!("{BASE}{GALLERY}/")
            } else {
                request.url.clone()
            },
            headers: BTreeMap::new(),
            body,
        })
    }
}
fn installed() -> (
    tempfile::TempDir,
    SourceRegistry,
    Source,
    Arc<FixtureTransport>,
) {
    let root = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(root.path()).unwrap();
    let descriptor = registry.install_zip(&archive()).unwrap();
    assert!(descriptor.nsfw);
    assert!(!descriptor.enabled);
    registry.set_enabled(ID, true).unwrap();
    let transport = Arc::new(FixtureTransport::default());
    let source = registry
        .source(ID)
        .unwrap()
        .with_transport(transport.clone());
    (root, registry, source, transport)
}
fn last_url(transport: &FixtureTransport) -> url::Url {
    url::Url::parse(&transport.requests.lock().unwrap().last().unwrap().url).unwrap()
}
#[test]
fn native_gallery_flow_preserves_identity_order_and_page_permissions() {
    let (_root, _registry, source, _transport) = installed();
    let capabilities = source.capabilities(&json!({})).unwrap();
    assert_eq!(capabilities["sources"][0]["id"], ID);
    assert_eq!(capabilities["sources"][0]["nsfw"], true);
    let listing = source
        .browse(&json!({"catalogue":HTML_CATALOGUE,"page":2}))
        .unwrap();
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].id, GALLERY);
    assert!(listing.has_next_page);
    assert_eq!(
        listing.items[0].cover_url.as_deref(),
        Some(format!("{BASE}/images/cover.svg").as_str())
    );
    let details = source
        .details(&json!({"id":GALLERY,"url":format!("{BASE}{GALLERY}/")}))
        .unwrap();
    assert_eq!(details.id, GALLERY);
    assert_eq!(details.title, "Clockwork Garden");
    assert_eq!(details.genres, ["Art", "Paper Models"]);
    assert_eq!(details.status.as_deref(), Some("completed"));
    let chapters = source
        .chapters(&json!({"seriesId":GALLERY}))
        .unwrap()
        .chapters;
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].id, GALLERY);
    assert_eq!(chapters[0].series_id, GALLERY);
    assert_eq!(chapters[0].number.as_deref(), Some("1"));
    assert_eq!(chapters[0].uploaded_at.as_deref(), Some("2026-10-04"));
    let pages = source
        .pages(&json!({"chapterId":chapters[0].id}))
        .unwrap()
        .pages;
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].id, "0");
    assert_eq!(pages[1].id, "1");
    assert_eq!(pages[0].url, format!("{BASE}/images/full-2.svg"));
    assert!(pages[1].url.ends_with("/images/full-1.svg"));
    assert_eq!(pages[0].headers["Referer"], format!("{BASE}{GALLERY}"));
    assert_eq!(pages[0].headers["Origin"], BASE);
    assert!(
        source
            .fetch_page(&pages[0].url, &pages[0].headers, &CancelToken::default())
            .unwrap()
            .starts_with(b"<svg")
    );
    assert!(
        source
            .fetch_page(
                "https://untrusted.invalid/image.svg",
                &pages[0].headers,
                &CancelToken::default()
            )
            .is_err()
    );
    assert!(
        source
            .fetch_page(
                &pages[0].url.replace("https:", "http:"),
                &pages[0].headers,
                &CancelToken::default()
            )
            .is_err()
    );
}
#[test]
fn native_actionable_errors_explicit_empty_and_no_incomplete_results() {
    let (_root, _registry, source, transport) = installed();
    for status in [401, 403, 404, 429, 500] {
        *transport.override_response.lock().unwrap() = Some((status, b"unavailable".to_vec()));
        assert!(matches!(
            (
                status,
                source
                    .browse(&json!({"catalogue":HTML_CATALOGUE}))
                    .unwrap_err()
            ),
            (401, SourceError::AuthenticationRequired)
                | (403, SourceError::ChallengeRequired)
                | (404, SourceError::NotFound)
                | (429, SourceError::RateLimited)
                | (500, SourceError::Network(_))
        ));
    }
    *transport.override_response.lock().unwrap() =
        Some((200, b"<html>Just a moment</html>".to_vec()));
    assert!(matches!(
        source.browse(&json!({"catalogue":HTML_CATALOGUE})),
        Err(SourceError::ChallengeRequired)
    ));
    *transport.override_response.lock().unwrap() =
        Some((200, b"<main>Changed markup</main>".to_vec()));
    for op in ["browse", "details", "chapters", "pages"] {
        assert!(matches!(
            source.call(op, &json!({"catalogue":HTML_CATALOGUE,"id":GALLERY})),
            Err(SourceError::ParseChanged(_))
        ));
    }
    *transport.override_response.lock().unwrap() = Some((200, fixture("empty.html")));
    let empty = source.browse(&json!({"catalogue":HTML_CATALOGUE})).unwrap();
    assert!(empty.items.is_empty());
    assert!(!empty.has_next_page);
    *transport.override_response.lock().unwrap() = Some((200, fixture("video.html")));
    assert!(
        matches!(source.pages(&json!({"id":GALLERY})),Err(SourceError::ParseChanged(message)) if message.contains("Video-only"))
    );
    let broken = String::from_utf8(fixture("details.html"))
        .unwrap()
        .replace("href=\"images/full-1.svg\"", "");
    *transport.override_response.lock().unwrap() = Some((200, broken.into_bytes()));
    assert!(matches!(
        source.pages(&json!({"id":GALLERY})),
        Err(SourceError::ParseChanged(_))
    ));
    let external = String::from_utf8(fixture("details.html"))
        .unwrap()
        .replace("/images/full-2.svg", "https://untrusted.invalid/image.svg");
    *transport.override_response.lock().unwrap() = Some((200, external.into_bytes()));
    assert!(matches!(
        source.pages(&json!({"id":GALLERY})),
        Err(SourceError::ParseChanged(_))
    ));
    let cancel = CancelToken::default();
    cancel.cancel();
    assert!(matches!(
        source.call_cancelled("browse", &json!({}), &cancel),
        Err(SourceError::Cancelled)
    ));
}
#[test]
fn native_search_precedence_tag_normalization_and_post_redirect() {
    let (_root, _registry, source, transport) = installed();
    source.search(&json!({"query":" clock & paper ","page":2,"filters":{"category":"nude","tag":"ignored"}})).unwrap();
    assert_eq!(
        last_url(&transport).as_str(),
        format!("{BASE}/page/2?s=clock%20%26%20paper")
    );
    source
        .search(&json!({"filters":{"category":"sexy-set","tag":"ignored"}}))
        .unwrap();
    assert_eq!(last_url(&transport).path(), "/category/sexy-set/page/1");
    source
        .search(&json!({"filters":{"tag":" Paper   Models "}}))
        .unwrap();
    assert_eq!(last_url(&transport).path(), "/tag/paper-models/page/1");
    let filters = source.call("filters", &json!({})).unwrap();
    assert_eq!(filters["filters"].as_array().unwrap().len(), 2);
    *transport.redirect_search.lock().unwrap() = true;
    let result = source.search(&json!({"query":"clock"})).unwrap();
    assert_eq!(result.items[0].id, GALLERY);
    assert!(!result.has_next_page);
    assert!(matches!(
        source.browse(&json!({"catalogue":"latest"})),
        Err(SourceError::NotFound)
    ));
}
