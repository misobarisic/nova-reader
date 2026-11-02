use reader_sources::{
    CancelToken, HttpRequest, HttpResponse, Result, SourceError, SourceRegistry, SourceTransport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    sync::{Arc, Mutex},
};

fn manifest() -> Value {
    json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"cookies","name":"Cookies","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"],"cookies":[{"origin":"https://example.com","names":["mature"]}]},"capabilities":["filters"],"configuration":[{"key":"mature","name":"Mature","kind":"bool","default":false}]})
}
fn package(manifest: &Value, script: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(manifest).unwrap()),
        ("main.js", script.as_bytes().to_vec()),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
const SET_COOKIE: &str = "globalThis.novaProvider={handle(r){return JSON.parse(nova.cookies.set('https://example.com','mature',r.settings.mature?'1':'0'));}};";
fn setup(script: &str) -> (tempfile::TempDir, SourceRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(dir.path()).unwrap();
    registry.install_zip(&package(&manifest(), script)).unwrap();
    registry.set_enabled("cookies", true).unwrap();
    (dir, registry)
}

#[test]
fn native_cookie_sessions_follow_live_settings_profiles_and_source_identity() {
    let (_dir, registry) = setup(SET_COOKIE);
    let source = registry.source("cookies").unwrap();
    let access = source.page_access();
    let url = "https://example.com/page.png";
    let headers = BTreeMap::new();
    let scope = || access.page_cache_scope(url, &headers).unwrap();
    let empty = scope();
    assert_eq!(
        source.call("filters", &json!({})).unwrap(),
        json!({"ok":true})
    );
    let off = scope();
    assert_ne!(empty, off);
    assert_eq!(
        off,
        registry
            .source("cookies")
            .unwrap()
            .page_cache_scope(url, &headers)
            .unwrap()
    );
    registry
        .set_settings("cookies", json!({"mature":true}))
        .unwrap();
    let updated = scope();
    assert_ne!(
        off, updated,
        "cache hits must see settings changes before scripts run"
    );
    source.call("filters", &json!({})).unwrap();
    let on = scope();
    assert_ne!(updated, on);
    registry
        .set_settings("cookies", json!({"mature":true}))
        .unwrap();
    assert_eq!(on, scope(), "unchanged patches retain cookie session");
    registry
        .set_settings("cookies", json!({"mature":false}))
        .unwrap();
    assert_ne!(on, scope());
    source.call("filters", &json!({})).unwrap();
    assert_ne!(on, scope());
    let other = registry.with_profile("other").source("cookies").unwrap();
    assert_ne!(scope(), other.page_cache_scope(url, &headers).unwrap());
    let other_empty = other.page_cache_scope(url, &headers).unwrap();
    source.call("filters", &json!({})).unwrap();
    assert_eq!(other_empty, other.page_cache_scope(url, &headers).unwrap());
    let mut second = manifest();
    second["id"] = json!("other-source");
    registry.install_zip(&package(&second, SET_COOKIE)).unwrap();
    registry.set_enabled("other-source", true).unwrap();
    let second = registry.source("other-source").unwrap();
    assert_ne!(scope(), second.page_cache_scope(url, &headers).unwrap());
    registry.disable("cookies").unwrap();
    assert!(matches!(
        access.page_cache_scope(url, &headers),
        Err(SourceError::Unavailable)
    ));
}

#[test]
fn cookie_manifest_validation_and_permission_expansion_are_local_grants() {
    let (dir, registry) = setup(SET_COOKIE);
    let base = manifest();
    for invalid in [
        json!([{"origin":"https://example.com/","names":["mature"]}]),
        json!([{"origin":"http://example.com","names":["mature"]}]),
        json!([{"origin":"https://example.com:443","names":["mature"]}]),
        json!([{"origin":"https://evil.example","names":["mature"]}]),
        json!([{"origin":"https://example.com","names":[]}]),
        json!([{"origin":"https://example.com","names":["x","x"]}]),
        json!([{"origin":"https://example.com","names":["a;b"]}]),
        json!([{"origin":"https://example.com","names":["x".repeat(129)]}]),
        json!([{"origin":"https://example.com","names":["mature"],"path":"/"}]),
        json!([{"origin":"https://example.com","names":["mature"]},{"origin":"https://example.com","names":["other"]}]),
        json!([{"origin":"https://example.com","names":(0..33).map(|n|n.to_string()).collect::<Vec<_>>()}]),
        json!(
            (0..17)
                .map(|n| json!({"origin":format!("https://s{n}.example.com"),"names":["mature"]}))
                .collect::<Vec<_>>()
        ),
    ] {
        let mut m = base.clone();
        m["permissions"]["cookies"] = invalid;
        assert!(SourceRegistry::inspect_archive(&package(&m, SET_COOKIE)).is_err());
    }
    let old = registry.source("cookies").unwrap();
    let mut expanded = base.clone();
    expanded["version"] = json!("1.1.0");
    expanded["permissions"]["cookies"][0]["names"] = json!(["mature", "other"]);
    assert!(
        !registry
            .install_zip(&package(&expanded, SET_COOKIE))
            .unwrap()
            .enabled
    );
    assert!(old.call("filters", &json!({})).is_err());
    assert!(registry.source("cookies").is_err());
    registry.set_enabled("cookies", true).unwrap();
    let mut narrowed = base.clone();
    narrowed["version"] = json!("1.2.0");
    assert!(
        registry
            .install_zip(&package(&narrowed, SET_COOKIE))
            .unwrap()
            .enabled
    );
    let mut origin = base;
    origin["version"] = json!("1.3.0");
    origin["permissions"]["cookies"][0]["origin"] = json!("https://cdn.example.com");
    assert!(
        !registry
            .install_zip(&package(&origin, SET_COOKIE))
            .unwrap()
            .enabled
    );
    drop(old);
    drop(registry);
    let registry = SourceRegistry::open(dir.path()).unwrap();
    assert!(
        registry.source("cookies").is_err(),
        "a peer/restart cannot restore a revoked local grant"
    );
}

struct CookieRecorder(Mutex<Vec<(String, String, String)>>);
impl SourceTransport for CookieRecorder {
    fn set_cookie(
        &self,
        origin: &str,
        name: &str,
        value: &str,
        cancel: &CancelToken,
    ) -> Result<()> {
        cancel.check()?;
        self.0
            .lock()
            .unwrap()
            .push((origin.into(), name.into(), value.into()));
        Ok(())
    }
    fn request(&self, input: &HttpRequest, _: usize, _: &CancelToken) -> Result<HttpResponse> {
        Ok(HttpResponse {
            status: 200,
            url: input.url.clone(),
            headers: BTreeMap::from([
                ("Set-Cookie".into(), "secret=private".into()),
                ("content-type".into(), "text/plain".into()),
            ]),
            ..Default::default()
        })
    }
}
struct CookieDenied;
impl SourceTransport for CookieDenied {
    fn request(&self, _: &HttpRequest, _: usize, _: &CancelToken) -> Result<HttpResponse> {
        panic!("denied cookie setting must stop before HTTP")
    }
}
#[test]
fn legacy_transport_denies_cookie_mutation_and_legacy_permissions_keep_serialization() {
    let permissions: reader_sources::Permissions =
        serde_json::from_value(json!({"domains":["example.com"],"httpPost":false})).unwrap();
    assert_eq!(
        serde_json::to_string(&permissions).unwrap(),
        r#"{"domains":["example.com"],"httpPost":false}"#
    );
    let (_dir, registry) = setup(SET_COOKIE);
    let source = registry
        .source("cookies")
        .unwrap()
        .with_transport(Arc::new(CookieDenied));
    assert!(matches!(
        source.call("filters", &json!({})),
        Err(SourceError::Network(_))
    ));
    let mut denied = manifest();
    denied["id"] = json!("no-cookies");
    denied["permissions"]
        .as_object_mut()
        .unwrap()
        .remove("cookies");
    registry.install_zip(&package(&denied, SET_COOKIE)).unwrap();
    registry.set_enabled("no-cookies", true).unwrap();
    let transport = Arc::new(CookieRecorder(Mutex::new(vec![])));
    assert!(
        registry
            .source("no-cookies")
            .unwrap()
            .with_transport(transport.clone())
            .call("filters", &json!({}))
            .is_err()
    );
    assert!(transport.0.lock().unwrap().is_empty());
}
#[test]
fn javascript_cookie_boundary_checks_permissions_limits_and_hides_cookie_reads() {
    let (_dir, registry) = setup(
        "globalThis.novaProvider={handle(r){const result=JSON.parse(nova.cookies.set(r.origin,r.name,r.value));return {result,read:typeof nova.cookies.get,all:typeof nova.cookies.all,headers:JSON.parse(nova.http.get('https://example.com')).headers};}};",
    );
    let transport = Arc::new(CookieRecorder(Mutex::new(vec![])));
    let source = registry
        .source("cookies")
        .unwrap()
        .with_transport(transport.clone());
    let valid = json!({"origin":"https://example.com","name":"mature","value":"1"});
    let result = source.call("filters", &valid).unwrap();
    assert_eq!(
        result,
        json!({"result":{"ok":true},"read":"undefined","all":"undefined","headers":{"content-type":"text/plain"}})
    );
    for patch in [
        json!({"origin":"https://cdn.example.com"}),
        json!({"name":"unlisted"}),
        json!({"value":"1; Secure"}),
        json!({"value":"x".repeat(4097)}),
    ] {
        let mut request = valid.clone();
        request
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(matches!(
            source.call("filters", &request),
            Err(SourceError::Network(_))
        ));
    }
    assert_eq!(
        transport.0.lock().unwrap().len(),
        1,
        "invalid cookies never reach custom transports"
    );
    let token = CancelToken::default();
    token.cancel();
    assert!(matches!(
        source.call_cancelled("filters", &valid, &token),
        Err(SourceError::Cancelled)
    ));
    assert_eq!(transport.0.lock().unwrap().len(), 1);
    let (_dir, registry) = setup(
        "globalThis.novaProvider={handle(){for(let i=0;i<33;i++)nova.cookies.set('https://example.com','mature','1');return {};}};",
    );
    let transport = Arc::new(CookieRecorder(Mutex::new(vec![])));
    assert!(
        registry
            .source("cookies")
            .unwrap()
            .with_transport(transport.clone())
            .call("filters", &json!({}))
            .is_err()
    );
    assert_eq!(transport.0.lock().unwrap().len(), 32);
}
