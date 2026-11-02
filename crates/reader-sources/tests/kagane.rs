//! Actual QuickJS/HTTP host fixtures: neutral synthetic records, no site images.
use reader_sources::{
    CancelToken, HttpRequest, HttpResponse, SourceError, SourceRegistry, SourceTransport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
fn uid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
fn archive() -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/addons/kagane");
    let m: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut names = vec!["manifest.json".to_owned(), "main.js".to_owned()];
    names.extend(
        m["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned()),
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in names {
        zip.start_file(&name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&std::fs::read(root.join(name)).unwrap())
            .unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[derive(Default)]
struct Fixture {
    requests: Mutex<Vec<HttpRequest>>,
    tokens: AtomicUsize,
    reject_pages: AtomicUsize,
    mode: Mutex<String>,
}
impl SourceTransport for Fixture {
    fn request(
        &self,
        r: &HttpRequest,
        _cap: usize,
        cancel: &CancelToken,
    ) -> reader_sources::Result<HttpResponse> {
        cancel.check()?;
        self.requests.lock().unwrap().push(r.clone());
        let u = url::Url::parse(&r.url).unwrap();
        let mode = self.mode.lock().unwrap().clone();
        // Reproduce the live image edge's 404 when Origin is absent. Metadata
        // remains available without Origin, as it did on the proxy used to probe
        // the site; the flow test separately checks upstream's XHR metadata rule.
        if (u.host_str() == Some("kstatic.to")
            && r.headers.get("Origin").map(String::as_str) != Some("https://kagane.moe"))
            || (mode == "not_found" && u.path() != "/")
        {
            return Ok(HttpResponse {
                status: 404,
                url: r.url.clone(),
                headers: BTreeMap::new(),
                body: b"{\"error\":\"not found\"}".to_vec(),
            });
        }
        let mut status = 200;
        let body = if u.host_str() == Some("kstatic.to") {
            let mut rejected = self.reject_pages.load(Ordering::SeqCst);
            let reject = loop {
                if rejected == 0 {
                    break false;
                }
                match self.reject_pages.compare_exchange(
                    rejected,
                    rejected - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break true,
                    Err(current) => rejected = current,
                }
            };
            if reject {
                status = 401;
                vec![]
            } else {
                b"synthetic image bytes".to_vec()
            }
        } else {
            assert_eq!(u.host_str(), Some("kagane.moe"));
            if u.path() == "/" {
                b"<html></html>".to_vec()
            } else {
                let data = match u.path() {
                    "/api/integrity" => {
                        assert_eq!(r.method, "POST");
                        json!({"token":"fixture-integrity","exp":9999999999u64})
                    }
                    "/api/v2/search/series" => {
                        assert_eq!(r.method, "POST");
                        json!({"last":false,"content":[{"series_id":uid(1),"title":"Clockmaker [Edition]","cover_image_id":uid(2)}]})
                    }
                    "/api/v2/genres/list" => json!([{"id":uid(8),"genre_name":"Adventure"}]),
                    "/api/v2/tags/list" => json!([{"id":uid(9),"tag_name":"Clockwork"}]),
                    "/api/v2/sources/list" => {
                        json!({"sources":[{"source_id":uid(10),"title":"Sample Press","source_type":"Official"}]})
                    }
                    "/api/v2/search/staff" => {
                        json!({"content":[{"staff_id":uid(11),"name":"Ada","native_name":null,"alternate_names":[]}]})
                    }
                    "/api/v2/groups/search" => {
                        json!({"items":[{"group_id":uid(12),"title":"Sample Group"}]})
                    }
                    path if path == format!("/api/v2/series/{}", uid(1)) => {
                        json!({"title":"Clockmaker [Edition]","edition_info":"First","description":"<p>Neutral <b>clock</b> story</p>","upload_status":"Ongoing","format":"Manhwa","series_staff":[{"staff_id":uid(11),"name":"Ada","role":"Author"}],"series_covers":[{"image_id":uid(2)}],"genres":[{"genre_name":"Adventure"}]})
                    }
                    path if path == format!("/api/v2/series/{}/books/index", uid(1)) => {
                        json!({"count":if mode=="incomplete" {3} else {2},"fields":["title","group_ix","chapter_no","book_id","volume_no","released_at","sort_no","page_count"],"groups":[{"title":"Sample Group"}],"books":[["First",[0],"1",uid(3),"1",1700000000,1,2],["Interlude",[0],"1.5",uid(4),"1",1700000001,2,2]]})
                    }
                    path if path == format!("/api/v2/books/{}", uid(3)) => {
                        assert_eq!(
                            r.headers.get("x-integrity-token").map(String::as_str),
                            Some("fixture-integrity")
                        );
                        let n = self.tokens.fetch_add(1, Ordering::SeqCst);
                        json!({"access_token":format!("fixture-token-{n}"),"cache_url":if mode=="evil" {"https://kstatic.to.evil.test"} else {"https://kstatic.to"},"manifest":{"pages":[{"page_no":1,"page_id":uid(6),"ext":"jxl"},{"page_no":0,"page_id":uid(5),"ext":"jxl"}]}})
                    }
                    path => panic!("unexpected fixture path {path}"),
                };
                serde_json::to_vec(&data).unwrap()
            }
        };
        Ok(HttpResponse {
            status,
            url: r.url.clone(),
            headers: BTreeMap::new(),
            body,
        })
    }
}
#[test]
fn kagane_native_flow_tokens_filters_and_complete_identity() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    registry.install_zip(&archive()).unwrap();
    registry.set_enabled("kagane", true).unwrap();
    assert!(registry.icon("kagane").unwrap().is_some());
    let t = Arc::new(Fixture::default());
    let source = registry.source("kagane").unwrap().with_transport(t.clone());
    let cap = source.call("capabilities", &json!({})).unwrap();
    assert_eq!(cap["sources"].as_array().unwrap().len(), 18);
    let list = source
        .browse(&json!({"sourceId":"kagane.ko","catalogue":"latest","page":2}))
        .unwrap();
    assert_eq!(list.items[0].id, uid(1));
    assert!(list.has_next_page);
    let r = t.requests.lock().unwrap().last().unwrap().clone();
    let body: Value = serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["content_lang"], json!(["ko"]));
    assert_eq!(body["age_rating"], json!(["Safe"]));
    assert_eq!(body["is_pornographic"], false);
    assert!(r.url.contains("page=1"));
    assert!(r.url.contains("updated_at%2Cdesc"));
    let details = source.details(&json!({"seriesId":uid(1)})).unwrap();
    assert_eq!(details.authors, vec!["Ada"]);
    assert!(details.description.unwrap().contains("Neutral clock story"));
    let chapters = source
        .chapters(&json!({"seriesId":uid(1),"sourceId":"kagane.en"}))
        .unwrap();
    assert_eq!(chapters.chapters[0].id, uid(4));
    assert_eq!(chapters.chapters[0].number.as_deref(), Some("1.5"));
    assert_eq!(
        chapters.chapters[0].scanlator.as_deref(),
        Some("Sample Group")
    );
    let definitions = source.call("filters", &json!({})).unwrap();
    assert!(
        definitions["filters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["type"] == "tristate")
    );
    source.search(&json!({"sourceId":"kagane.en","query":"Clock","filters":{"genres":{"include":[uid(8)],"exclude":[]},"sources":[uid(10)],"tags":"Clockwork","person":"Ada","group":"Sample Group","year":"2024-2026","chapterCount":"10+"}})).unwrap();
    let r = t.requests.lock().unwrap().last().unwrap().clone();
    let body: Value = serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["genres"]["values"], json!([uid(8)]));
    assert_eq!(body["tags"]["values"], json!([uid(9)]));
    assert_eq!(body["staff"]["values"], json!([uid(11)]));
    assert_eq!(body["groups"]["values"], json!([uid(12)]));
    assert_eq!(body["start_year"], json!({"min":2024,"max":2026}));
    assert_eq!(body["current_books"], json!({"min":10}));
    let pages = source.pages(&json!({"chapterId":uid(3)})).unwrap();
    assert_eq!(pages.pages[0].id, uid(5));
    assert!(pages.pages[0].url.contains(".jxl?token="));
    assert_eq!(pages.pages[0].headers["Origin"], "https://kagane.moe");
    assert_eq!(pages.pages[0].headers["Referer"], "https://kagane.moe/");
    t.reject_pages.store(1, Ordering::SeqCst);
    assert_eq!(
        source
            .fetch_page(
                &pages.pages[0].url,
                &pages.pages[0].headers,
                &CancelToken::default()
            )
            .unwrap(),
        b"synthetic image bytes"
    );
    assert_eq!(t.tokens.load(Ordering::SeqCst), 2);
    t.reject_pages.store(2, Ordering::SeqCst);
    assert!(matches!(
        source.fetch_page(
            &pages.pages[0].url,
            &pages.pages[0].headers,
            &CancelToken::default()
        ),
        Err(SourceError::AuthenticationRequired)
    ));
    assert_eq!(t.tokens.load(Ordering::SeqCst), 3, "one retry only");
    *t.mode.lock().unwrap() = "evil".into();
    assert!(matches!(
        source.pages(&json!({"chapterId":uid(3)})),
        Err(SourceError::ParseChanged(_))
    ));
    *t.mode.lock().unwrap() = "incomplete".into();
    assert!(matches!(
        source.chapters(&json!({"seriesId":uid(1)})),
        Err(SourceError::ParseChanged(_))
    ));
    let cancel = CancelToken::default();
    cancel.cancel();
    assert!(matches!(
        source.call_cancelled("browse", &json!({}), &cancel),
        Err(SourceError::Cancelled)
    ));
    registry.set_settings("kagane",json!({"contentRating":"eighteenplus","showPornographic":true,"showDisturbing":true,"dataSaver":true,"cleanTitle":true})).unwrap();
    source.browse(&json!({})).unwrap();
    let r = t.requests.lock().unwrap().last().unwrap().clone();
    let body: Value = serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["age_rating"],
        json!(["Safe", "FifteenPlus", "EighteenPlus"])
    );
    assert!(body.get("is_pornographic").is_none());
    assert!(t.requests.lock().unwrap().iter().all(|request| {
        request.headers.get("Origin").map(String::as_str) == Some("https://kagane.moe")
    }));
}

#[test]
fn old_package_404_regression_and_retired_mirror_settings_migrate_on_upgrade() {
    let old_zip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/kagane-1.0.0.zip");
    let old_bytes = std::fs::read(old_zip).unwrap();
    for retired in ["kagane.cx", "kagane.net"] {
        let directory = tempfile::tempdir().unwrap();
        let registry = SourceRegistry::open(directory.path()).unwrap();
        registry.install_zip(&old_bytes).unwrap();
        registry.set_enabled("kagane", true).unwrap();
        let transport = Arc::new(Fixture::default());
        let old_source = registry
            .source("kagane")
            .unwrap()
            .with_transport(transport.clone());
        old_source.browse(&json!({})).unwrap();
        let old_pages = old_source.pages(&json!({"chapterId":uid(3)})).unwrap();
        assert!(matches!(
            old_source.fetch_page(
                &old_pages.pages[0].url,
                &old_pages.pages[0].headers,
                &CancelToken::default()
            ),
            Err(SourceError::NotFound)
        ));
        registry
            .set_settings(
                "kagane",
                json!({"mirror":retired,"dataSaver":true,"cleanTitle":true}),
            )
            .unwrap();
        transport.requests.lock().unwrap().clear();

        let descriptor = registry.install_zip(&archive()).unwrap();
        assert!(
            descriptor.enabled,
            "unchanged domain grants must retain activation"
        );
        assert!(matches!(
            old_source.browse(&json!({})),
            Err(SourceError::Unavailable)
        ));
        let settings = registry.settings("kagane").unwrap();
        assert_eq!(settings["mirror"], "kagane.moe");
        assert_eq!(settings["dataSaver"], true);
        assert_eq!(settings["cleanTitle"], true);
        let source = registry
            .source("kagane")
            .unwrap()
            .with_transport(transport.clone());
        let listing = source.browse(&json!({})).unwrap();
        assert_eq!(listing.items[0].title, "Clockmaker");
        source.search(&json!({"query":"Clock"})).unwrap();
        source.filters(&json!({})).unwrap();
        source.details(&json!({"seriesId":uid(1)})).unwrap();
        source.chapters(&json!({"seriesId":uid(1)})).unwrap();
        let pages = source.pages(&json!({"chapterId":uid(3)})).unwrap();
        assert!(pages.pages[0].url.contains("/page/datasaver/"));
        assert_eq!(pages.pages[0].headers["Origin"], "https://kagane.moe");
        let requests = transport.requests.lock().unwrap();
        assert!(requests.iter().all(|r| {
            let url = url::Url::parse(&r.url).unwrap();
            url.host_str() == Some("kagane.moe")
        }));
        assert!(!requests.iter().any(|r| r.url.contains(retired)));
    }
}

#[test]
fn fixed_endpoint_404_reports_protocol_change_but_missing_series_remains_not_found() {
    let directory = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(directory.path()).unwrap();
    registry.install_zip(&archive()).unwrap();
    registry.set_enabled("kagane", true).unwrap();
    let transport = Arc::new(Fixture::default());
    *transport.mode.lock().unwrap() = "not_found".into();
    let source = registry.source("kagane").unwrap().with_transport(transport);
    for op in ["browse", "search", "filters", "pages"] {
        assert!(
            matches!(
                source.call(op, &json!({"chapterId":uid(3)})),
                Err(SourceError::ParseChanged(_))
            ),
            "{op}"
        );
    }
    for op in ["details", "chapters"] {
        assert!(
            matches!(
                source.call(op, &json!({"seriesId":uid(1)})),
                Err(SourceError::NotFound)
            ),
            "{op}"
        );
    }
}

#[test]
#[ignore = "manual live Safe metadata/manifest smoke; site availability is external"]
fn live_safe_metadata_and_manifest_smoke() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    registry.install_zip(&archive()).unwrap();
    registry.set_enabled("kagane", true).unwrap();
    let source = registry.source("kagane").unwrap();
    let list = source
        .search(&json!({"sourceId":"kagane.en","query":"Yotsuba","page":1}))
        .unwrap();
    let series = list.items.first().expect("safe query returned no series");
    source.details(&json!({"seriesId":series.id})).unwrap();
    let chapters = source.chapters(&json!({"seriesId":series.id})).unwrap();
    let chapter = chapters.chapters.first().expect("no chapter metadata");
    let pages = source.pages(&json!({"chapterId":chapter.id})).unwrap();
    assert!(!pages.pages.is_empty());
    // Do not request any live image bytes or print signed page URLs/tokens.
}
