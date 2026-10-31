use reader_sources::{
    CancelToken, HttpRequest, HttpResponse, Result, SourceError, SourceRegistry, SourceTransport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    sync::{Arc, Mutex},
};
use zip::write::SimpleFileOptions;
fn archive(files: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn manifest() -> Value {
    json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"fixture","name":"Fixture","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"]},"capabilities":["browse","search","filters","details","chapters","pages"]})
}
fn package(m: &Value, script: &str) -> Vec<u8> {
    archive(vec![
        ("manifest.json".into(), serde_json::to_vec(m).unwrap()),
        ("main.js".into(), script.as_bytes().to_vec()),
    ])
}
fn load(root: &std::path::Path, name: &str) -> Vec<u8> {
    let dir = root.join("../../examples/addons").join(name);
    let m: Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    let mut names = vec![
        "manifest.json".to_string(),
        m["entry"].as_str().unwrap().into(),
    ];
    for key in ["scripts", "assets"] {
        for value in m[key].as_array().unwrap() {
            names.push(value.as_str().unwrap().into())
        }
    }
    archive(
        names
            .into_iter()
            .map(|n| {
                let bytes = std::fs::read(dir.join(&n)).unwrap();
                (n, bytes)
            })
            .collect(),
    )
}
#[test]
fn neutral_package_drives_complete_actual_host_flow_and_assets() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let bytes = load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        "neutral-fixture",
    );
    let installed = registry.install_zip(&bytes).unwrap();
    assert!(!installed.enabled);
    assert!(matches!(
        registry.source(&installed.id),
        Err(SourceError::Unavailable)
    ));
    registry.set_enabled(&installed.id, true).unwrap();
    let source = registry.source(&installed.id).unwrap();
    let listing = source.browse(&json!({"page":1})).unwrap();
    assert_eq!(listing.items.len(), 1);
    let series = source.details(&json!({"id":listing.items[0].id})).unwrap();
    let chapters = source.chapters(&json!({"id":series.id})).unwrap();
    assert!(chapters.chapters.len() >= 4);
    let pages = source
        .pages(&json!({"chapterId":"/fixture/hundreds"}))
        .unwrap();
    assert_eq!(pages.pages.len(), 320);
    let page = &pages.pages[0];
    let bytes = source
        .fetch_page(&page.url, &page.headers, &CancelToken::default())
        .unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert!(matches!(
        source.fetch_page(
            "addon://neutral-fixture/manifest.json",
            &BTreeMap::new(),
            &CancelToken::default()
        ),
        Err(SourceError::NotFound)
    ));
    drop(source);
    drop(registry);
    let registry = SourceRegistry::open(dir.path()).unwrap();
    assert!(registry.list().unwrap()[0].enabled);
}
#[test]
fn immutable_updates_atomic_activation_and_permission_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let mut m = manifest();
    let script = "globalThis.novaProvider={handle:r=>({value:1})};";
    let bytes = package(&m, script);
    registry.install_zip(&bytes).unwrap();
    registry.set_enabled("fixture", true).unwrap();
    let old = registry.source("fixture").unwrap();
    assert!(
        registry
            .install_zip(&package(
                &m,
                "globalThis.novaProvider={handle:r=>({value:2})};"
            ))
            .is_err()
    );
    assert_eq!(old.call("filters", &json!({})).unwrap()["value"], 1);
    m["version"] = json!("1.1.0");
    m["permissions"]["domains"] = json!(["example.com", "other.example"]);
    assert!(!registry.install_zip(&package(&m, script)).unwrap().enabled);
    assert!(old.call("filters", &json!({})).is_err());
    registry.set_enabled("fixture", true).unwrap();
    registry.remove("fixture").unwrap();
    assert!(
        registry
            .install_zip(&package(
                &manifest(),
                "globalThis.novaProvider={handle(){return 2}};"
            ))
            .is_err()
    );
}
#[test]
fn archive_contract_rejects_unsafe_or_incompatible_packages() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    for (key, value) in [
        ("formatVersion", json!(1)),
        ("contract", json!("stremio")),
        ("api", json!(">=2")),
        ("version", json!("yesterday")),
        ("entry", json!("../main.js")),
        ("scripts", json!(["main.js"])),
        ("permissions", json!({"domains":["*"]})),
    ] {
        let mut m = manifest();
        m[key] = value;
        assert!(
            registry.install_zip(&package(&m, "void 0;")).is_err(),
            "{key}"
        );
    }
    for extra in ["../escape", "/absolute", "x\\y", "unlisted.txt"] {
        let bytes = archive(vec![
            (
                "manifest.json".into(),
                serde_json::to_vec(&manifest()).unwrap(),
            ),
            ("main.js".into(), b"void 0;".to_vec()),
            (extra.into(), vec![]),
        ]);
        assert!(registry.install_zip(&bytes).is_err());
    }
    assert!(
        registry
            .install_zip(&package(&manifest(), "function {"))
            .is_err()
    );
    let mut bomb = manifest();
    bomb["assets"] = json!(["huge.txt"]);
    let bytes = archive(vec![
        ("manifest.json".into(), serde_json::to_vec(&bomb).unwrap()),
        ("main.js".into(), b"void 0;".to_vec()),
        ("huge.txt".into(), vec![0; 9 * 1024 * 1024]),
    ]);
    assert!(registry.install_zip(&bytes).is_err());
}
#[test]
fn registry_holds_exclusive_process_lock() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    assert!(SourceRegistry::open(dir.path()).is_err());
    let clone = registry.clone();
    drop(registry);
    assert!(SourceRegistry::open(dir.path()).is_err());
    drop(clone);
    assert!(SourceRegistry::open(dir.path()).is_ok());
}
struct FixtureTransport {
    body: String,
    status: u16,
    seen: Mutex<Vec<HttpRequest>>,
}
impl SourceTransport for FixtureTransport {
    fn request(&self, r: &HttpRequest, _: usize, cancel: &CancelToken) -> Result<HttpResponse> {
        cancel.check()?;
        self.seen.lock().unwrap().push(r.clone());
        Ok(HttpResponse {
            status: self.status,
            url: r.url.clone(),
            body: self.body.as_bytes().to_vec(),
            headers: BTreeMap::new(),
        })
    }
}
fn source_with_script(script: &str) -> (tempfile::TempDir, SourceRegistry, reader_sources::Source) {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    registry.install_zip(&package(&manifest(), script)).unwrap();
    registry.set_enabled("fixture", true).unwrap();
    let source = registry.source("fixture").unwrap();
    (dir, registry, source)
}
#[test]
fn runtime_is_fresh_bounded_and_has_no_process_capabilities() {
    let (_dir, registry, source) = source_with_script(
        "let count=0;globalThis.novaProvider={handle(){return [++count,typeof process,typeof require,typeof std,typeof os,typeof fetch];}};",
    );
    let result = source.call("filters", &json!({})).unwrap();
    assert_eq!(
        result,
        json!([
            1,
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ])
    );
    assert_eq!(source.call("filters", &json!({})).unwrap(), result);
    let cancel = CancelToken::default();
    cancel.cancel();
    assert!(matches!(
        source.call_cancelled("filters", &json!({}), &cancel),
        Err(SourceError::Cancelled)
    ));
    registry.disable("fixture").unwrap();
    assert!(source.call("filters", &json!({})).is_err());
    registry.set_enabled("fixture", true).unwrap();
    assert!(source.call("filters", &json!({})).is_err());
    let (_dir, _registry, source) =
        source_with_script("globalThis.novaProvider={handle(){return 'x'.repeat(3*1024*1024)}}");
    assert!(source.call("filters", &json!({})).is_err());
    let (_dir, _registry, source) =
        source_with_script("globalThis.novaProvider={handle(){return 'x'.repeat(64*1024*1024)}}");
    assert!(source.call("filters", &json!({})).is_err());
}
#[test]
fn http_and_html_helpers_use_actual_host_and_deny_unlisted_hosts() {
    let (_dir, _registry, source) = source_with_script(
        r#"globalThis.novaProvider={handle(){const r=JSON.parse(nova.http.get('https://example.com/list','{}'));return JSON.parse(nova.html.select(r.body,'li',JSON.stringify({title:{selector:'a',value:'ownText'},url:{selector:'a',value:'href'}})));}}"#,
    );
    let transport = Arc::new(FixtureTransport {
        body: "<li><a href='/a'>One<span>18+</span></a></li>".into(),
        status: 200,
        seen: Mutex::new(vec![]),
    });
    let source = source.with_transport(transport.clone());
    assert_eq!(
        source.call("filters", &json!({})).unwrap(),
        json!([{"title":"One","url":"/a"}])
    );
    assert_eq!(transport.seen.lock().unwrap().len(), 1);
    let (_dir, _registry, source) = source_with_script(
        "globalThis.novaProvider={handle(){return JSON.parse(nova.http.get('https://evil.example/','{}'))}};",
    );
    assert!(matches!(
        source
            .with_transport(transport.clone())
            .call("filters", &json!({})),
        Err(SourceError::Network(_))
    ));
    assert_eq!(transport.seen.lock().unwrap().len(), 1);
}
#[test]
fn request_count_and_header_policy_enforced_before_custom_transport() {
    for script in [
        "globalThis.novaProvider={handle(){for(let i=0;i<13;i++)nova.http.get('https://example.com/','{}');return {};}};",
        "globalThis.novaProvider={handle(){return nova.http.get('https://example.com/',JSON.stringify({Host:'evil.example'}));}};",
    ] {
        let (_dir, _registry, source) = source_with_script(script);
        let transport = Arc::new(FixtureTransport {
            body: "ok".into(),
            status: 200,
            seen: Mutex::new(vec![]),
        });
        assert!(matches!(
            source
                .with_transport(transport.clone())
                .call("filters", &json!({})),
            Err(SourceError::Network(_))
        ));
        assert!(transport.seen.lock().unwrap().len() <= 12);
    }
}
#[test]
fn manhwa_parser_executes_through_quickjs_with_sanitized_html() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    registry.install_zip(&load(root, "manhwa18cc")).unwrap();
    registry.set_enabled("manhwa18cc", true).unwrap();
    for (op, file, request) in [
        (
            "browse",
            "list",
            json!({"sourceId":"manhwa18cc.en","page":1}),
        ),
        ("details", "details", json!({"id":"/webtoon/clockwork"})),
        ("chapters", "details", json!({"id":"/webtoon/clockwork"})),
        (
            "pages",
            "pages",
            json!({"id":"/webtoon/clockwork/chapter-2.5"}),
        ),
    ] {
        let fixture = std::fs::read_to_string(root.join(format!(
            "../../examples/addons/manhwa18cc/tests/fixtures/{file}.html"
        )))
        .unwrap();
        let transport = Arc::new(FixtureTransport {
            body: fixture,
            status: 200,
            seen: Mutex::new(vec![]),
        });
        let source = registry
            .source("manhwa18cc")
            .unwrap()
            .with_transport(transport);
        let value = source.call(op, &request).unwrap();
        match op {
            "browse" => assert!(!value["items"].as_array().unwrap().is_empty()),
            "details" => assert_eq!(value["title"], "Clockwork"),
            "chapters" => {
                assert_eq!(value["chapters"][0]["number"], "2.5");
                assert_eq!(value["chapters"][1]["number"], Value::Null);
            }
            "pages" => assert_eq!(value["pages"].as_array().unwrap().len(), 3),
            _ => unreachable!(),
        }
    }
    for (status, code) in [
        (401, "authentication"),
        (403, "challenge"),
        (429, "rate limited"),
    ] {
        let source = registry
            .source("manhwa18cc")
            .unwrap()
            .with_transport(Arc::new(FixtureTransport {
                body: "".into(),
                status,
                seen: Mutex::new(vec![]),
            }));
        assert!(
            source
                .call("browse", &json!({}))
                .unwrap_err()
                .to_string()
                .contains(code)
        );
    }
    let source = registry
        .source("manhwa18cc")
        .unwrap()
        .with_transport(Arc::new(FixtureTransport {
            body: "<div>changed</div>".into(),
            status: 200,
            seen: Mutex::new(vec![]),
        }));
    assert!(matches!(
        source.call("browse", &json!({})),
        Err(SourceError::ParseChanged(_))
    ));
}

#[test]
fn script_cancellation_interrupts_runaway_and_image_statuses_remain_typed() {
    let (_dir, _registry, source) =
        source_with_script("globalThis.novaProvider={handle(){while(true){}}}");
    let cancel = CancelToken::default();
    let trigger = cancel.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        source.call_cancelled("filters", &json!({}), &cancel),
        Err(SourceError::Cancelled)
    ));
    worker.join().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    for status in [401, 403, 404, 429] {
        let source = source.clone().with_transport(Arc::new(FixtureTransport {
            body: String::new(),
            status,
            seen: Mutex::new(vec![]),
        }));
        let error = source
            .fetch_page(
                "https://example.com/image",
                &BTreeMap::new(),
                &CancelToken::default(),
            )
            .unwrap_err();
        assert!(match status {
            401 => matches!(error, SourceError::AuthenticationRequired),
            403 => matches!(error, SourceError::ChallengeRequired),
            404 => matches!(error, SourceError::NotFound),
            429 => matches!(error, SourceError::RateLimited),
            _ => false,
        });
    }
}

#[test]
fn rejects_symlinks_and_invalid_output_identities() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let mut m = manifest();
    m["assets"] = json!(["link"]);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in [
        ("manifest.json", serde_json::to_vec(&m).unwrap()),
        ("main.js", b"void 0;".to_vec()),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.add_symlink("link", "/etc/passwd", SimpleFileOptions::default())
        .unwrap();
    assert!(
        registry
            .install_zip(&zip.finish().unwrap().into_inner())
            .is_err()
    );
    for (op, response, request) in [
        (
            "browse",
            json!({"items":[{"id":"a","title":"A"},{"id":"a","title":"duplicate"}]}),
            json!({}),
        ),
        (
            "details",
            json!({"id":"different","title":"Other"}),
            json!({"id":"requested"}),
        ),
        (
            "chapters",
            json!({"chapters":[{"id":"c","seriesId":"wrong","title":"C","sourceOrder":0}]}),
            json!({"seriesId":"requested"}),
        ),
        (
            "pages",
            json!({"pages":[{"id":"0","url":"https://evil.test/image"}]}),
            json!({}),
        ),
        ("pages", json!({"pages":[]}), json!({})),
    ] {
        let script = format!("globalThis.novaProvider={{handle(){{return {response};}}}};");
        let (_dir, _registry, source) = source_with_script(&script);
        assert!(source.call(op, &request).is_err(), "{op}");
    }
}

#[test]
fn duplicate_central_directory_names_are_rejected_before_zip_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let mut m = manifest();
    m["assets"] = json!(["else.js"]);
    let mut bytes = archive(vec![
        ("manifest.json".into(), serde_json::to_vec(&m).unwrap()),
        ("main.js".into(), b"void 0;".to_vec()),
        ("else.js".into(), b"void 0;".to_vec()),
    ]);
    for i in 0..bytes.len() - 7 {
        if &bytes[i..i + 7] == b"else.js" {
            bytes[i..i + 7].copy_from_slice(b"main.js");
        }
    }
    assert!(registry.install_zip(&bytes).is_err());
}

fn configured_manifest() -> Value {
    let mut m = manifest();
    m["configuration"] = json!([
        {"key":"label","name":"Label","kind":"text","default":"Default"},
        {"key":"enabled","name":"Enabled","kind":"bool","default":false},
        {"key":"mode","name":"Mode","kind":"select","default":"popular","options":["popular","latest"]},
        {"key":"token","name":"Token","kind":"secret","default":""}
    ]);
    m
}
#[test]
fn settings_are_typed_profile_scoped_durable_and_host_owned() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let script = "globalThis.novaProvider={handle(r){return {settings:r.settings,label:nova.settings.get('label'),enabled:nova.settings.get('enabled'),token:nova.settings.get('token'),unknown:nova.settings.get('unknown')};}};";
    registry
        .install_zip(&package(&configured_manifest(), script))
        .unwrap();
    registry.set_enabled("fixture", true).unwrap();
    let source = registry.source("fixture").unwrap();
    registry
        .set_settings(
            "fixture",
            json!({"label":"Mine","enabled":true,"mode":"latest","token":"private-value"}),
        )
        .unwrap();
    let safe = registry.settings("fixture").unwrap();
    assert_eq!(safe, json!({"label":"Mine","enabled":true,"mode":"latest"}));
    assert_eq!(
        registry.settings_secrets("fixture").unwrap().get("token"),
        Some(&true)
    );
    let result = source
        .call(
            "filters",
            &json!({"settings":{"label":"untrusted","enabled":"bad","injected":"bad"}}),
        )
        .unwrap();
    assert_eq!(result["label"], "Mine");
    assert_eq!(result["enabled"], true);
    assert_eq!(result["token"], "private-value");
    assert_eq!(result["settings"]["token"], "private-value");
    assert!(result["settings"].get("injected").is_none());
    assert!(result["unknown"].is_null());
    registry
        .set_settings("fixture", json!({"label":"Changed"}))
        .unwrap();
    assert_eq!(
        registry.settings_secrets("fixture").unwrap().get("token"),
        Some(&true)
    );
    for patch in [
        json!({"unknown":"bad"}),
        json!({"enabled":"true"}),
        json!({"mode":"other"}),
        json!({"token":null}),
        json!({"label":"x".repeat(4097)}),
        json!([]),
    ] {
        assert!(registry.set_settings("fixture", patch).is_err());
    }
    assert_eq!(registry.settings("fixture").unwrap()["label"], "Changed");
    let other = registry.with_profile("../other-profile");
    assert_eq!(other.settings("fixture").unwrap()["label"], "Default");
    assert_eq!(
        other.settings_secrets("fixture").unwrap().get("token"),
        Some(&false)
    );
    other
        .set_settings("fixture", json!({"label":"Other"}))
        .unwrap();
    assert_eq!(registry.settings("fixture").unwrap()["label"], "Changed");
    let descriptor = serde_json::to_string(&registry.list().unwrap()).unwrap();
    assert!(!descriptor.contains("private-value"));
    assert!(!descriptor.contains("token"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for profile in std::fs::read_dir(dir.path().join("settings")).unwrap() {
            for file in std::fs::read_dir(profile.unwrap().path()).unwrap() {
                assert_eq!(
                    std::fs::metadata(file.unwrap().path())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }
    drop(source);
    drop(other);
    drop(registry);
    let registry = SourceRegistry::open(dir.path()).unwrap();
    assert_eq!(registry.settings("fixture").unwrap()["label"], "Changed");
    assert_eq!(
        registry.settings_secrets("fixture").unwrap().get("token"),
        Some(&true)
    );
    assert_eq!(
        registry
            .with_profile("../other-profile")
            .settings("fixture")
            .unwrap()["label"],
        "Other"
    );
    registry
        .set_settings("fixture", json!({"token":""}))
        .unwrap();
    assert_eq!(
        registry.settings_secrets("fixture").unwrap().get("token"),
        Some(&false)
    );
}
#[test]
fn settings_schema_rejects_invalid_defaults_and_secrets_are_redacted_in_errors() {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    let mut m = configured_manifest();
    for configuration in [
        json!([{"key":"token","name":"Token","kind":"secret","default":"embedded-credential"}]),
        json!([{"key":"a","name":"A","kind":"bool","default":"true"}]),
        json!([{"key":"a","name":"A","kind":"select","default":"x","options":["y"]}]),
        json!([{"key":"a","name":"A","kind":"text","default":""},{"key":"a","name":"Again","kind":"text","default":""}]),
    ] {
        m["configuration"] = configuration;
        assert!(registry.install_zip(&package(&m, "void 0;")).is_err());
    }
    let m = configured_manifest();
    registry
        .install_zip(&package(
            &m,
            "globalThis.novaProvider={handle(){throw Error(nova.settings.get('token'));}};",
        ))
        .unwrap();
    registry.set_enabled("fixture", true).unwrap();
    registry
        .set_settings("fixture", json!({"token":"  private-value  "}))
        .unwrap();
    let error = registry
        .source("fixture")
        .unwrap()
        .call("filters", &json!({}))
        .unwrap_err()
        .to_string();
    assert!(!error.contains("private-value"));
    assert!(error.contains("[redacted]"));
    // Reclassifying a secret as ordinary text must reset its value, never expose it.
    let mut updated = configured_manifest();
    updated["version"] = json!("1.1.0");
    updated["configuration"][3]["kind"] = json!("text");
    updated["configuration"][3]["default"] = json!("public-default");
    registry
        .install_zip(&package(
            &updated,
            "globalThis.novaProvider={handle(){return {};}};",
        ))
        .unwrap();
    assert_eq!(
        registry.settings("fixture").unwrap()["token"],
        "public-default"
    );
}
