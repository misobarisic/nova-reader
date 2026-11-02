//! Real QuickJS/registry coverage with newly authored neutral HTML/XML fixtures.
use reader_sources::{
    CancelToken, HttpRequest, HttpResponse, Source, SourceError, SourceRegistry, SourceTransport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const BASE: &str = "https://toonily.com";
const SERIES: &str = "/serie/clockwork-garden";
const CHAPTER: &str = "/serie/clockwork-garden/chapter-2-5";

fn addon_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/addons/toonily")
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
    cookies: Mutex<Vec<(String, String, String)>>,
    override_response: Mutex<Option<(u16, Vec<u8>)>>,
    single_pager: Mutex<bool>,
    token: Mutex<String>,
}
impl SourceTransport for FixtureTransport {
    fn set_cookie(
        &self,
        origin: &str,
        name: &str,
        value: &str,
        cancel: &CancelToken,
    ) -> reader_sources::Result<()> {
        cancel.check()?;
        assert_eq!(origin, BASE);
        assert_eq!(name, "toonily-mature");
        assert!(["0", "1"].contains(&value));
        self.cookies
            .lock()
            .unwrap()
            .push((origin.into(), name.into(), value.into()));
        Ok(())
    }
    fn request(
        &self,
        request: &HttpRequest,
        _max_bytes: usize,
        cancel: &CancelToken,
    ) -> reader_sources::Result<HttpResponse> {
        cancel.check()?;
        assert!(!self.cookies.lock().unwrap().is_empty());
        assert!(
            !request
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("cookie"))
        );
        self.requests.lock().unwrap().push(request.clone());
        let url = url::Url::parse(&request.url).unwrap();
        let (status, body) = if let Some(response) = self.override_response.lock().unwrap().clone()
        {
            response
        } else if url.host_str() == Some("data.tnlycdn.com") {
            assert_eq!(request.headers["Referer"], format!("{BASE}{CHAPTER}/"));
            (200, vec![1, 2, 3])
        } else {
            let body = match url.path() {
                "/serie/" if url.query().is_none() => fixture("genres.html"),
                "/wp-manga-tag-sitemap.xml" => fixture("tags.xml"),
                "/serie/clockwork-garden/" => fixture("details.html"),
                "/serie/clockwork-garden/ajax/chapters/" => {
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.body.as_deref(), Some(""));
                    assert_eq!(request.headers["X-Requested-With"], "XMLHttpRequest");
                    assert_eq!(request.headers["Referer"], format!("{BASE}{SERIES}/"));
                    fixture("chapters.html")
                }
                "/serie/clockwork-garden/chapter-2-5/" => {
                    if *self.single_pager.lock().unwrap() && url.query() != Some("style=list") {
                        b"<div id=\"single-pager\"></div>".to_vec()
                    } else {
                        let token = self.token.lock().unwrap();
                        String::from_utf8(fixture("pages.html"))
                            .unwrap()
                            .replace("token=old", &format!("token={token}"))
                            .into_bytes()
                    }
                }
                path if path.starts_with("/serie/")
                    || path.starts_with("/search/")
                    || path.starts_with("/tag/") =>
                {
                    fixture("list.html")
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
    let directory = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(directory.path()).unwrap();
    let descriptor = registry.install_zip(&archive()).unwrap();
    assert!(descriptor.nsfw);
    assert_eq!(descriptor.id, "toonily");
    registry.set_enabled("toonily", true).unwrap();
    let transport = Arc::new(FixtureTransport::default());
    let source = registry
        .source("toonily")
        .unwrap()
        .with_transport(transport.clone());
    (directory, registry, source, transport)
}
fn last_url(transport: &FixtureTransport) -> url::Url {
    url::Url::parse(&transport.requests.lock().unwrap().last().unwrap().url).unwrap()
}
fn override_body(transport: &FixtureTransport, body: &str) {
    *transport.override_response.lock().unwrap() = Some((200, body.as_bytes().to_vec()));
}

#[test]
fn native_identity_pagination_search_slug_filters_and_tag_precedence() {
    let (_directory, _registry, source, transport) = installed();
    let capabilities = source.capabilities(&json!({})).unwrap();
    assert_eq!(capabilities["sources"][0]["id"], "toonily.en");
    assert_eq!(capabilities["sources"][0]["language"], "en");
    assert_eq!(capabilities["sources"][0]["nsfw"], true);
    assert!(transport.cookies.lock().unwrap().is_empty());
    let listing = source.browse(&json!({"page":2})).unwrap();
    assert_eq!(listing.items[0].id, SERIES);
    assert!(listing.has_next_page);
    assert_eq!(
        last_url(&transport).as_str(),
        format!("{BASE}/serie/page/2/?m_orderby=views")
    );
    source.browse(&json!({"catalogue":"latest"})).unwrap();
    assert_eq!(last_url(&transport).query(), Some("m_orderby=latest"));
    source.search(&json!({"page":3,"query":"  Clock's GARDEN!! café  ","filters":{"author":" Ada ","artist":" Sam ","release":"2026","status":["end","on-hold"],"sort":"rating","adult":"0","genreCondition":"1","genres":["clockwork","adventure"]}})).unwrap();
    let url = last_url(&transport);
    assert_eq!(url.path(), "/search/clock-s-garden-caf/page/3/");
    let pairs = url.query_pairs().collect::<Vec<_>>();
    assert!(pairs.iter().any(|(k, v)| k == "author" && v == "Ada"));
    assert!(pairs.iter().any(|(k, v)| k == "artist" && v == "Sam"));
    assert!(pairs.iter().any(|(k, v)| k == "status[]" && v == "end"));
    assert!(
        pairs
            .iter()
            .any(|(k, v)| k == "genre[]" && v == "adventure")
    );
    assert!(pairs.iter().any(|(k, v)| k == "adult" && v == "0"));
    source.search(&json!({"query":"Ignored","page":2,"filters":{"tag":"mechanical-garden","sort":"latest","author":"Ignored","genres":["ignored"],"adult":"1"}})).unwrap();
    assert_eq!(
        last_url(&transport).as_str(),
        format!("{BASE}/tag/mechanical-garden/page/2/?m_orderby=latest")
    );
    let filters = source.filters(&json!({})).unwrap();
    let definitions = filters["filters"].as_array().unwrap();
    let tags = definitions.iter().find(|f| f["id"] == "tag").unwrap();
    assert_eq!(tags["options"][1]["value"], "clock");
    assert_eq!(tags["options"].as_array().unwrap().len(), 3);
    assert!(matches!(
        source.browse(&json!({"sourceId":"toonily.ko"})),
        Err(SourceError::NotFound)
    ));
}

#[test]
fn details_stable_paths_order_decimal_numbers_and_complete_page_headers() {
    let (_directory, _registry, source, transport) = installed();
    let series = source
        .details(&json!({"id":SERIES,"url":"/webtoon/clockwork-garden/"}))
        .unwrap();
    assert_eq!(series.id, SERIES);
    assert_eq!(series.title, "Clockwork Garden");
    assert_eq!(series.authors, ["Ada", "Sam"]);
    assert_eq!(series.genres, ["Adventure", "Clockwork", "Webtoon"]);
    assert_eq!(series.status.as_deref(), Some("on_hiatus"));
    assert!(
        series
            .description
            .unwrap()
            .ends_with("Alternative names: The Clock Garden")
    );
    let chapters = source
        .chapters(&json!({"seriesId":SERIES}))
        .unwrap()
        .chapters;
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].id, CHAPTER);
    assert_eq!(chapters[0].number.as_deref(), Some("2.5"));
    assert_eq!(chapters[1].number, None);
    assert_eq!(chapters[0].uploaded_at.as_deref(), Some("2026-10-09"));
    assert_eq!(chapters[1].uploaded_at.as_deref(), Some("2026-10-08"));
    assert_eq!(
        chapters.iter().map(|c| c.source_order).collect::<Vec<_>>(),
        [0, 1]
    );
    let first = source.pages(&json!({"chapterId":CHAPTER})).unwrap().pages;
    assert_eq!(first.len(), 7);
    assert_eq!(first[0].id, format!("{CHAPTER}/page/0"));
    assert_eq!(first[0].headers["Referer"], format!("{BASE}{CHAPTER}/"));
    assert_eq!(first[5].url, format!("{BASE}/neutral/panel-6.svg"));
    assert_eq!(
        source
            .fetch_page(&first[0].url, &first[0].headers, &CancelToken::default())
            .unwrap(),
        [1, 2, 3]
    );
    *transport.token.lock().unwrap() = "new".into();
    *transport.single_pager.lock().unwrap() = true;
    let second = source.pages(&json!({"chapterId":CHAPTER})).unwrap().pages;
    assert_eq!(second.len(), 7);
    assert_eq!(last_url(&transport).query(), Some("style=list"));
    assert!(second[0].url.ends_with("token=new"));
    assert_eq!(
        first.iter().map(|p| &p.id).collect::<Vec<_>>(),
        second.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    override_body(&transport, "<ul class=\"main version-chap\"></ul>");
    assert!(
        source
            .chapters(&json!({"id":SERIES}))
            .unwrap()
            .chapters
            .is_empty()
    );
    override_body(
        &transport,
        "<div class=\"no-results\">No matching titles</div>",
    );
    let empty = source.browse(&json!({})).unwrap();
    assert!(empty.items.is_empty());
    assert!(!empty.has_next_page);
}

#[test]
fn mature_setting_is_host_owned_defaults_false_and_updates_existing_source_handles() {
    let (_directory, registry, source, transport) = installed();
    assert_eq!(
        registry.settings("toonily").unwrap(),
        json!({"showMature":false})
    );
    source
        .browse(&json!({"settings":{"showMature":true}}))
        .unwrap();
    assert_eq!(transport.cookies.lock().unwrap().last().unwrap().2, "0");
    registry
        .set_settings("toonily", json!({"showMature":true}))
        .unwrap();
    for (op, request) in [
        ("browse", json!({})),
        ("search", json!({"query":"garden"})),
        ("filters", json!({})),
        ("details", json!({"id":SERIES})),
        ("chapters", json!({"id":SERIES})),
        ("pages", json!({"id":CHAPTER})),
    ] {
        let before = transport.cookies.lock().unwrap().len();
        source.call(op, &request).unwrap();
        let cookies = transport.cookies.lock().unwrap();
        assert_eq!(cookies.len(), before + 1, "{op}");
        assert_eq!(cookies.last().unwrap().2, "1", "{op}");
    }
    registry
        .set_settings("toonily", json!({"showMature":false}))
        .unwrap();
    source.search(&json!({"query":"garden"})).unwrap();
    assert_eq!(transport.cookies.lock().unwrap().last().unwrap().2, "0");
    assert!(
        registry
            .set_settings("toonily", json!({"showMature":"true"}))
            .is_err()
    );
}

#[test]
fn actionable_status_errors_changed_markup_and_protected_readers_fail_explicitly() {
    let (_directory, _registry, source, transport) = installed();
    for status in [401, 403, 404, 429, 500] {
        *transport.override_response.lock().unwrap() = Some((status, b"error".to_vec()));
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
    override_body(&transport, "<html><title>Just a moment...</title></html>");
    assert!(matches!(
        source.browse(&json!({})),
        Err(SourceError::ChallengeRequired)
    ));
    override_body(&transport, "<div>Unrecognized markup</div>");
    for (op, request) in [
        ("browse", json!({})),
        ("details", json!({"id":SERIES})),
        ("chapters", json!({"id":SERIES})),
        ("pages", json!({"id":CHAPTER})),
        ("filters", json!({})),
    ] {
        assert!(
            matches!(source.call(op, &request), Err(SourceError::ParseChanged(_))),
            "{op}"
        );
    }
    for body in [
        "<div id=\"single-pager\"></div>",
        "<div class=\"page-break\"><img src=\"/neutral.svg\"></div><script id=\"chapter-protector-data\">encrypted</script>",
        "<div class=\"page-break\"><img src=\"/neutral.svg\"></div><div class=\"page-break\"><img></div>",
        "<div class=\"reading-content\"><div class=\"text-left\"><img src=\"/1.svg\"><img src=\"/2.svg\"></div></div>",
    ] {
        override_body(&transport, body);
        assert!(matches!(
            source.pages(&json!({"id":CHAPTER})),
            Err(SourceError::ParseChanged(_))
        ));
    }
    override_body(
        &transport,
        "<div class=\"page-break\"><img src=\"https://denied.invalid/panel.svg\"></div>",
    );
    assert!(source.pages(&json!({"id":CHAPTER})).is_err());
    let cancel = CancelToken::default();
    cancel.cancel();
    assert!(matches!(
        source.call_cancelled("browse", &json!({}), &cancel),
        Err(SourceError::Cancelled)
    ));
}
