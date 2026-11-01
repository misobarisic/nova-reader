//! Native MangaFire package contracts using only newly authored synthetic JSON.
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

fn addon_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/addons/mangafire")
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
                .map(|value| value.as_str().unwrap().to_owned()),
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
    no_volumes: Mutex<bool>,
    author_found: Mutex<bool>,
}
impl SourceTransport for FixtureTransport {
    fn request(
        &self,
        request: &HttpRequest,
        _cap: usize,
        cancel: &CancelToken,
    ) -> reader_sources::Result<HttpResponse> {
        cancel.check()?;
        self.requests.lock().unwrap().push(request.clone());
        let url = url::Url::parse(&request.url).unwrap();
        let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
        if url.host_str() == Some("pw6.mfcdn1.xyz") {
            assert!(request.headers.contains_key("Referer"));
            return Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                headers: BTreeMap::new(),
                body: vec![1, 2, 3],
            });
        }
        assert_eq!(
            request.headers.get("Accept").map(String::as_str),
            Some("application/json")
        );
        assert!(pairs.get("vrf").is_some_and(|v| !v.is_empty()));
        let (status, body) = if let Some(response) = self.override_response.lock().unwrap().clone()
        {
            response
        } else {
            let body = match url.path() {
                "/api/titles" => fixture("list.json"),
                "/api/titles/clock1" => fixture("details.json"),
                "/api/titles/clock1/chapters" => {
                    fixture(if pairs.get("page").is_some_and(|v| v == "2") {
                        "chapters-2.json"
                    } else {
                        "chapters-1.json"
                    })
                }
                "/api/titles/clock1/volumes" => {
                    if *self.no_volumes.lock().unwrap() {
                        br#"{"items":[]}"#.to_vec()
                    } else {
                        fixture("volumes.json")
                    }
                }
                "/api/chapters/101" | "/api/chapters/102" | "/api/volumes/501"
                | "/api/volumes/502" => fixture("pages.json"),
                "/api/tags" => {
                    if *self.author_found.lock().unwrap() {
                        br#"{"data":[{"id":45,"type":"author"}]}"#.to_vec()
                    } else {
                        br#"{"data":[]}"#.to_vec()
                    }
                }
                path => panic!("unexpected fixture path {path}"),
            };
            (200, body)
        };
        Ok(HttpResponse {
            status,
            url: request.url.clone(),
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
    registry.set_enabled("mangafire", true).unwrap();
    let transport = Arc::new(FixtureTransport::default());
    let source = registry
        .source("mangafire")
        .unwrap()
        .with_transport(transport.clone());
    (root, registry, source, transport)
}
fn last_url(transport: &FixtureTransport) -> url::Url {
    url::Url::parse(&transport.requests.lock().unwrap().last().unwrap().url).unwrap()
}
#[test]
fn native_package_browse_search_details_languages_and_settings() {
    let (_root, registry, source, transport) = installed();
    let capabilities = source.capabilities(&json!({})).unwrap();
    assert_eq!(capabilities["sources"].as_array().unwrap().len(), 7);
    assert!(
        capabilities["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["nsfw"] == true)
    );
    let listing = source.browse(&json!({"page":2})).unwrap();
    assert!(listing.has_next_page);
    assert_eq!(listing.items[0].id, "/title/clock1");
    assert_eq!(
        listing.items[0].cover_url.as_deref(),
        Some("https://static.mfcdn.nl/covers/large.png")
    );
    let url = last_url(&transport);
    let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(pairs["page"], "2");
    assert_eq!(pairs["order[views_30d]"], "desc");
    assert_eq!(pairs["content_rating[]"], "safe");
    source
        .browse(&json!({"catalogue":"latest","sourceId":"mangafire.es-419"}))
        .unwrap();
    let url = last_url(&transport);
    let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(pairs["languages[]"], "es-la");
    assert_eq!(pairs["order[chapter_updated_at]"], "desc");
    source.search(&json!({"sourceId":"mangafire.pt-BR","query":" café ","filters":{"genres":{"include":["1"],"exclude":["78"]},"types":["manga"],"matchMode":"or","sort":"title:asc","minChapter":"2"}})).unwrap();
    let url = last_url(&transport);
    let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(pairs["keyword"], "café");
    assert_eq!(pairs["languages[]"], "pt-br");
    assert_eq!(pairs["genres_in[]"], "1");
    assert_eq!(pairs["genres_ex[]"], "78");
    assert_eq!(pairs["order[title]"], "asc");
    let count = transport.requests.lock().unwrap().len();
    assert!(
        source
            .search(&json!({"filters":{"author":"Missing"}}))
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(transport.requests.lock().unwrap().len(), count + 1);
    *transport.author_found.lock().unwrap() = true;
    source.search(&json!({"filters":{"author":"Ada"}})).unwrap();
    assert_eq!(
        last_url(&transport)
            .query_pairs()
            .find(|(k, _)| k == "authors[]")
            .unwrap()
            .1,
        "45"
    );
    let details = source.details(&json!({"id":"/title/clock1"})).unwrap();
    assert_eq!(details.id, "/title/clock1");
    assert_eq!(details.status.as_deref(), Some("on_hiatus"));
    assert_eq!(details.authors, ["Ada"]);
    assert_eq!(details.genres, ["Manga", "Adventure", "Time Travel"]);
    assert_eq!(
        details.description.as_deref(),
        Some("A clockmaker tends a mechanical garden.")
    );
    registry
        .set_settings("mangafire", json!({"contentRating":"all"}))
        .unwrap();
    source.browse(&json!({})).unwrap();
    assert!(
        !last_url(&transport)
            .query_pairs()
            .any(|(k, _)| k == "content_rating[]")
    );
    assert!(
        source.filters(&json!({})).unwrap()["filters"]
            .as_array()
            .unwrap()
            .len()
            > 5
    );
}
#[test]
fn chapters_all_pages_edition_order_and_volume_preferences() {
    let (_root, registry, source, transport) = installed();
    let chapters = source
        .chapters(&json!({"id":"/title/clock1"}))
        .unwrap()
        .chapters;
    assert_eq!(chapters.len(), 4);
    assert_eq!(chapters[0].number.as_deref(), Some("2.5"));
    assert_eq!(chapters[1].scanlator.as_deref(), Some("official"));
    assert!(chapters[3].id.contains("9223372036854775807"));
    assert_eq!(
        chapters.iter().map(|c| c.source_order).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        chapters[0].uploaded_at.as_deref(),
        Some("2026-10-09T00:00:00.000Z")
    );
    let spanish = source
        .chapters(&json!({"id":"/title/clock1","sourceId":"mangafire.es-419"}))
        .unwrap();
    assert_eq!(spanish.chapters[0].language.as_deref(), Some("es-419"));
    assert!(spanish.chapters[0].id.ends_with("-es-la"));
    registry
        .set_settings("mangafire", json!({"mergeChapters":true}))
        .unwrap();
    let merged = source
        .chapters(&json!({"id":"/title/clock1"}))
        .unwrap()
        .chapters;
    assert_eq!(merged.len(), 3);
    assert!(merged[0].id.contains("/102-"));
    registry
        .set_settings("mangafire", json!({"preferOfficial":false}))
        .unwrap();
    assert!(
        source
            .chapters(&json!({"id":"/title/clock1"}))
            .unwrap()
            .chapters[0]
            .id
            .contains("/101-")
    );
    registry
        .set_settings("mangafire", json!({"preferVolumes":true}))
        .unwrap();
    let volumes = source
        .chapters(&json!({"id":"/title/clock1","sourceId":"mangafire.fr"}))
        .unwrap()
        .chapters;
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].id, "/title/clock1/volume/502");
    *transport.no_volumes.lock().unwrap() = true;
    assert_eq!(
        source
            .chapters(&json!({"id":"/title/clock1"}))
            .unwrap()
            .chapters
            .len(),
        3
    );
}
#[test]
fn ordered_pages_cdn_permissions_referer_and_dimensions() {
    let (_root, _registry, source, transport) = installed();
    let pages = source
        .pages(&json!({"chapterId":"/title/clock1/101-chapter-2.5-en"}))
        .unwrap()
        .pages;
    assert_eq!(
        pages.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["0", "1"]
    );
    assert!(pages[0].url.contains("second.png"));
    assert!(pages[1].url.contains("first.webp"));
    assert_eq!(pages[1].height, Some(12000));
    assert_eq!(
        pages[0].headers["Referer"],
        "https://mangafire.to/title/clock1/101-chapter-2.5-en"
    );
    assert_eq!(
        source
            .fetch_page(&pages[0].url, &pages[0].headers, &CancelToken::default())
            .unwrap(),
        [1, 2, 3]
    );
    source
        .pages(&json!({"chapterId":"/title/clock1/volume/501"}))
        .unwrap();
    assert_eq!(last_url(&transport).path(), "/api/volumes/501");
    *transport.override_response.lock().unwrap() = Some((
        200,
        br#"{"data":{"pages":[{"url":"https://untrusted.invalid/page.png"}]}}"#.to_vec(),
    ));
    assert!(matches!(
        source.pages(&json!({"chapterId":"/title/clock1/101-chapter-2.5-en"})),
        Err(SourceError::ParseChanged(_))
    ));
}
#[test]
fn actionable_errors_markup_changes_and_no_partial_chapter_lists() {
    let (_root, _registry, source, transport) = installed();
    for status in [401, 403, 404, 429, 500] {
        *transport.override_response.lock().unwrap() = Some((status, b"{}".to_vec()));
        let error = source.browse(&json!({})).unwrap_err();
        assert!(matches!(
            (status, error),
            (401, SourceError::AuthenticationRequired)
                | (403, SourceError::ChallengeRequired)
                | (404, SourceError::NotFound)
                | (429, SourceError::RateLimited)
                | (500, SourceError::Network(_))
        ));
    }
    *transport.override_response.lock().unwrap() =
        Some((200, br#"{"error":"captcha_required"}"#.to_vec()));
    assert!(matches!(
        source.browse(&json!({})),
        Err(SourceError::ChallengeRequired)
    ));
    *transport.override_response.lock().unwrap() = Some((200, fixture("challenge-phrase.json")));
    assert_eq!(
        source.browse(&json!({})).unwrap().items[0].title,
        "Just a Moment: Verify You Are Human"
    );
    let mut details: Value = serde_json::from_slice(&fixture("details.json")).unwrap();
    details["data"]["synopsisHtml"] = json!("<p>Just a moment; the clockmaker waits.</p>");
    *transport.override_response.lock().unwrap() =
        Some((200, serde_json::to_vec(&details).unwrap()));
    assert_eq!(
        source
            .details(&json!({"id":"/title/clock1"}))
            .unwrap()
            .description
            .as_deref(),
        Some("Just a moment; the clockmaker waits.")
    );
    *transport.override_response.lock().unwrap() =
        Some((200, b"<html>Just a moment</html>".to_vec()));
    assert!(matches!(
        source.browse(&json!({})),
        Err(SourceError::ChallengeRequired)
    ));
    for body in ["<html>markup changed</html>", "{}", "{\"items\":[{}]}"] {
        *transport.override_response.lock().unwrap() = Some((200, body.as_bytes().to_vec()));
        assert!(matches!(
            source.browse(&json!({})),
            Err(SourceError::ParseChanged(_))
        ));
    }
    *transport.override_response.lock().unwrap() =
        Some((200, br#"{"items":[],"meta":{"hasNext":true}}"#.to_vec()));
    let listing = source.browse(&json!({})).unwrap();
    assert!(listing.items.is_empty());
    assert!(listing.has_next_page);
    *transport.override_response.lock().unwrap() =
        Some((200, br#"{"items":[],"meta":{"lastPage":13}}"#.to_vec()));
    assert!(matches!(
        source.chapters(&json!({"id":"/title/clock1"})),
        Err(SourceError::ParseChanged(_))
    ));
    let cancel = CancelToken::default();
    cancel.cancel();
    assert!(matches!(
        source.call_cancelled("browse", &json!({}), &cancel),
        Err(SourceError::Cancelled)
    ));
}

/// Explicit opt-in only: metadata/page descriptors, never image bytes or a captcha solver.
#[test]
#[ignore = "manual live safe-metadata smoke; network/challenge availability is external"]
fn live_safe_metadata_smoke() {
    let root = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(root.path()).unwrap();
    registry.install_zip(&archive()).unwrap();
    registry.set_enabled("mangafire", true).unwrap();
    let source = registry.source("mangafire").unwrap();
    let listing = source.browse(&json!({"sourceId":"mangafire.en"})).unwrap();
    assert!(!listing.items.is_empty());
    let series = source.details(&json!({"id":listing.items[0].id})).unwrap();
    let chapters = source
        .chapters(&json!({"id":series.id,"sourceId":"mangafire.en"}))
        .unwrap();
    assert!(!chapters.chapters.is_empty());
    let pages = source
        .pages(&json!({"chapterId":chapters.chapters[0].id}))
        .unwrap();
    assert!(!pages.pages.is_empty());
    println!(
        "MangaFire safe-metadata smoke: listing/details/{} chapters/{} page descriptors; no image bytes fetched",
        chapters.chapters.len(),
        pages.pages.len()
    );
}
