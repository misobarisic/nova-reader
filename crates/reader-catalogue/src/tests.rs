use super::*;
use ed25519_dalek::{Signer, SigningKey};
use flate2::{Compression, write::GzEncoder};
use semver::Version;
use std::{cell::RefCell, collections::VecDeque, io::Write, rc::Rc};

const FIXTURE: &[u8] = include_bytes!("../fixtures/keiyoushi-manhwa18.pb.gz");
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
fn signed() -> (Vec<u8>, SigningKey, TrustStore) {
    let bytes = br#"{"format_version":1,"repository":"reader-reference","sequence":7,"packages":[{"id":"manhwa18cc","name":"Manhwa18.cc","version":"0.1.0","api":"^1","zip_url":"https://example.org/manhwa18cc.zip","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sources":[{"id":"manhwa18cc.en","name":"Manhwa18.cc","language":"en","nsfw":true,"capabilities":["browse","search","details","chapters","pages"]}]}]}"#.to_vec();
    let key = SigningKey::from_bytes(&[71; 32]);
    let mut trust = TrustStore::new();
    trust
        .trust_key("reader-reference", "test", key.verifying_key().to_bytes())
        .unwrap();
    (bytes, key, trust)
}
fn verify(bytes: &[u8], key: &SigningKey, trust: &TrustStore) -> Result<NativeCatalogue> {
    verify_native_catalogue(
        bytes,
        "test",
        &hex::encode(key.sign(bytes).to_bytes()),
        trust,
    )
}

#[test]
fn real_upstream_fixture_has_exact_ids_and_metadata() {
    let catalogue = decode_keiyoushi_gzip(KEIYOUSHI_INDEX, FIXTURE).unwrap();
    assert_eq!(catalogue.name, "Keiyoushi");
    assert_eq!(catalogue.extensions.len(), 1);
    let extension = &catalogue.extensions[0];
    assert_eq!(
        extension.package_name,
        "eu.kanade.tachiyomi.extension.all.manhwa18cc"
    );
    assert_eq!(extension.version_name, "1.6.66");
    assert_eq!(extension.content_warning, 3);
    let ids: BTreeMap<_, _> = extension
        .sources
        .iter()
        .map(|s| (s.language.as_str(), s.id.as_str()))
        .collect();
    assert_eq!(ids["en"], "4841602236575491202");
    assert_eq!(ids["ko"], "6864643444164576223");
    assert_eq!(ids["all"], "993336618407081332");
    assert!(
        extension
            .sources
            .iter()
            .all(|s| s.home_url == "https://manhwa18.cc")
    );
    let json = serde_json::to_value(catalogue).unwrap();
    assert!(json["extensions"][0]["sources"][0]["id"].is_string());
}
#[test]
fn protobuf_unknown_fields_and_extreme_int64_ids_are_lossless() {
    let mut raw = decompress_gzip(FIXTURE).unwrap();
    // Unknown varint field 120, value 7.
    raw.extend_from_slice(&[0xc0, 0x07, 0x07]);
    assert!(decode_keiyoushi_gzip(KEIYOUSHI_INDEX, &gzip(&raw)).is_ok());
    let mut index = proto::Index::decode(raw.as_slice()).unwrap();
    let Some(proto::Extensions::Embedded(ref mut list)) = index.extensions else {
        panic!()
    };
    list.extensions[0].sources[0].id = i64::MAX;
    list.extensions[0].sources[1].id = i64::MIN;
    let parsed = decode_keiyoushi_gzip(KEIYOUSHI_INDEX, &gzip(&index.encode_to_vec())).unwrap();
    assert_eq!(parsed.extensions[0].sources[0].id, "9223372036854775807");
    assert_eq!(parsed.extensions[0].sources[1].id, "-9223372036854775808");
}
#[test]
fn limits_reject_compressed_and_expansion_bombs_and_truncated_gzip() {
    assert!(matches!(
        decompress_gzip(&vec![0; COMPRESSED_LIMIT + 1]),
        Err(CatalogueError::Limit)
    ));
    assert!(matches!(
        decompress_gzip(&gzip(&vec![0; EXPANDED_LIMIT + 1])),
        Err(CatalogueError::Limit)
    ));
    assert!(decompress_gzip(&FIXTURE[..FIXTURE.len() - 8]).is_err());
    let concatenated = [
        gzip(&vec![0; EXPANDED_LIMIT / 2 + 1]),
        gzip(&vec![0; EXPANDED_LIMIT / 2 + 1]),
    ]
    .concat();
    assert!(matches!(
        decompress_gzip(&concatenated),
        Err(CatalogueError::Limit)
    ));
}
#[test]
fn exact_bytes_bad_signatures_unknown_keys_and_rotation() {
    let (bytes, key, mut trust) = signed();
    let signature = hex::encode(key.sign(&bytes).to_bytes());
    assert!(verify_native_catalogue(&bytes, "test", &signature, &trust).is_ok());
    let mut changed = bytes.clone();
    changed.push(b' ');
    assert!(matches!(
        verify_native_catalogue(&changed, "test", &signature, &trust),
        Err(CatalogueError::Signature)
    ));
    assert!(matches!(
        verify_native_catalogue(&bytes, "unknown", &signature, &trust),
        Err(CatalogueError::UnknownKey(_))
    ));
    assert!(
        trust
            .trust_key("reader-reference", "test", key.verifying_key().to_bytes())
            .is_err()
    );
    assert!(trust.remove_key("test"));
    assert!(verify(&bytes, &key, &trust).is_err());
    trust
        .trust_key("reader-reference", "test", key.verifying_key().to_bytes())
        .unwrap();
    trust.advance_sequence("reader-reference", 8);
    trust.advance_sequence("reader-reference", 6);
    assert!(
        verify(&bytes, &key, &trust)
            .unwrap_err()
            .to_string()
            .contains("rollback")
    );
}
#[test]
fn native_schema_and_compatibility_are_validated_after_signature() {
    let (bytes, key, trust) = signed();
    let mut catalogue = verify(&bytes, &key, &trust).unwrap();
    let package = &mut catalogue.packages[0];
    assert_eq!(
        native_state(package, None, &Version::new(1, 0, 0)),
        NativeState::Available
    );
    assert_eq!(
        native_state(
            package,
            Some(&Version::new(0, 0, 1)),
            &Version::new(1, 0, 0)
        ),
        NativeState::UpdateAvailable
    );
    assert!(matches!(
        native_state(package, None, &Version::new(2, 0, 0)),
        NativeState::Incompatible { .. }
    ));
    package.sources[0]
        .capabilities
        .push("futureOperation".into());
    assert!(matches!(
        native_state(package, None, &Version::new(1, 0, 0)),
        NativeState::Incompatible { .. }
    ));
    for bad in [
        String::from_utf8(bytes.clone())
            .unwrap()
            .replace("https://example.org", "http://example.org"),
        String::from_utf8(bytes.clone())
            .unwrap()
            .replace("\"format_version\":1", "\"format_version\":2"),
        String::from_utf8(bytes.clone())
            .unwrap()
            .replace(&"a".repeat(64), "bad-sha"),
    ] {
        assert!(verify(bad.as_bytes(), &key, &trust).is_err());
    }
}
#[test]
fn port_listing_requires_matching_available_native_package() {
    let (bytes, key, trust) = signed();
    let catalogue = verify(&bytes, &key, &trust).unwrap();
    let mappings = reference_port_mappings();
    let installed = BTreeMap::new();
    let get = |id| {
        upstream_state(
            KEIYOUSHI_INDEX,
            "eu.kanade.tachiyomi.extension.all.manhwa18cc",
            id,
            &mappings,
            &catalogue,
            &installed,
        )
    };
    assert!(matches!(
        get("4841602236575491202"),
        UpstreamState::PortAvailable { .. }
    ));
    assert!(matches!(
        get("6864643444164576223"),
        UpstreamState::NotYetPorted { .. }
    ));
    let installed = BTreeMap::from([("manhwa18cc".into(), "0.1.0".into())]);
    assert!(matches!(
        upstream_state(
            KEIYOUSHI_INDEX,
            "eu.kanade.tachiyomi.extension.all.manhwa18cc",
            "4841602236575491202",
            &mappings,
            &catalogue,
            &installed
        ),
        UpstreamState::InstalledNativePort { .. }
    ));
}

type Requests = Rc<RefCell<Vec<Option<String>>>>;

struct Mock {
    responses: RefCell<VecDeque<FetchResponse>>,
    requests: Requests,
}
impl Transport for Mock {
    fn fetch(&self, _: &str, etag: Option<&str>) -> Result<FetchResponse> {
        self.requests.borrow_mut().push(etag.map(str::to_owned));
        Ok(self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("unexpected request"))
    }
}
fn response(bytes: Vec<u8>) -> FetchResponse {
    FetchResponse {
        status: 200,
        etag: Some("\"fixture-v1\"".into()),
        bytes,
    }
}
fn mock(responses: Vec<FetchResponse>) -> (CatalogueClient<Mock>, Requests) {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let client = CatalogueClient::with_transport(Mock {
        responses: RefCell::new(responses.into()),
        requests: requests.clone(),
    });
    (client, requests)
}
fn linked(url: &str) -> Vec<u8> {
    gzip(
        &proto::Index {
            name: "Linked".into(),
            extensions: Some(proto::Extensions::Url(url.into())),
            ..Default::default()
        }
        .encode_to_vec(),
    )
}
#[test]
fn conditional_fetch_reuses_304_and_retains_good_cache_after_bad_refresh() {
    let (mut client, requests) = mock(vec![
        response(FIXTURE.into()),
        response(vec![0; 12]),
        FetchResponse {
            status: 304,
            etag: None,
            bytes: vec![],
        },
    ]);
    client.fetch_keiyoushi(KEIYOUSHI_INDEX).unwrap();
    assert!(client.fetch_keiyoushi(KEIYOUSHI_INDEX).is_err());
    assert_eq!(
        client
            .fetch_keiyoushi(KEIYOUSHI_INDEX)
            .unwrap()
            .extensions
            .len(),
        1
    );
    assert_eq!(
        *requests.borrow(),
        [
            None,
            Some("\"fixture-v1\"".into()),
            Some("\"fixture-v1\"".into())
        ]
    );
}
#[test]
fn linked_indexes_and_bare_extension_lists_are_bounded() {
    let index = proto::Index::decode(decompress_gzip(FIXTURE).unwrap().as_slice()).unwrap();
    let Some(proto::Extensions::Embedded(list)) = index.extensions else {
        panic!()
    };
    let (mut client, _) = mock(vec![
        response(linked("next.pb")),
        response(gzip(&list.encode_to_vec())),
    ]);
    assert_eq!(
        client
            .fetch_keiyoushi(KEIYOUSHI_INDEX)
            .unwrap()
            .extensions
            .len(),
        1
    );
    let (mut client, _) = mock(vec![response(linked(KEIYOUSHI_INDEX))]);
    assert!(matches!(
        client.fetch_keiyoushi(KEIYOUSHI_INDEX),
        Err(CatalogueError::Recursion)
    ));
    let (mut client, _) = mock(
        (0..MAX_DEPTH)
            .map(|i| response(linked(&format!("next-{i}.pb"))))
            .collect(),
    );
    assert!(matches!(
        client.fetch_keiyoushi(KEIYOUSHI_INDEX),
        Err(CatalogueError::Recursion)
    ));
    let (mut client, _) = mock(vec![response(linked("http://example.org/list"))]);
    assert!(client.fetch_keiyoushi(KEIYOUSHI_INDEX).is_err());
}

#[test]
fn protobuf_empty_record_amplification_is_rejected_before_decode() {
    let list = proto::ExtensionList {
        extensions: vec![proto::Extension::default(); MAX_RECORDS + 1],
    };
    let index = proto::Index {
        extensions: Some(proto::Extensions::Embedded(list)),
        ..Default::default()
    };
    assert!(matches!(
        decode_keiyoushi_gzip(KEIYOUSHI_INDEX, &gzip(&index.encode_to_vec())),
        Err(CatalogueError::Limit)
    ));
}
#[test]
fn trusted_keys_are_bound_to_repository_identity() {
    let (bytes, key, trust) = signed();
    let wrong_repository = String::from_utf8(bytes)
        .unwrap()
        .replace("reader-reference", "another-publisher");
    assert!(
        verify(wrong_repository.as_bytes(), &key, &trust)
            .unwrap_err()
            .to_string()
            .contains("different repository")
    );
}
