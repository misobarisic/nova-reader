//! Byte-preserved live publisher metadata, not an executable APK/JAR fixture.
use reader_catalogue::{
    KEIYOUSHI_INDEX, NativeCatalogue, UpstreamState, decode_keiyoushi_gzip,
    reference_port_mappings, upstream_state,
};
use std::collections::BTreeMap;

#[test]
fn exact_mangafire_upstream_ids_match_native_language_variants() {
    let bytes = include_bytes!("../fixtures/keiyoushi-mangafire.pb.gz");
    let catalogue = decode_keiyoushi_gzip(KEIYOUSHI_INDEX, bytes).unwrap();
    assert_eq!(catalogue.name, "Keiyoushi");
    assert_eq!(catalogue.extensions.len(), 1);
    let extension = &catalogue.extensions[0];
    assert_eq!(
        extension.package_name,
        "eu.kanade.tachiyomi.extension.all.mangafire"
    );
    assert_eq!(extension.version_code, "106035");
    assert_eq!(extension.version_name, "1.6.35");
    assert_eq!(extension.content_warning, 2);
    let expected = BTreeMap::from([
        ("en", "6084907896154116083"),
        ("es", "8098539940032331958"),
        ("es-419", "3849324770075062403"),
        ("fr", "6228021941803932286"),
        ("ja", "524110931382140732"),
        ("pt", "758400875161895515"),
        ("pt-BR", "7842137233968841729"),
    ]);
    let mappings = reference_port_mappings();
    assert_eq!(extension.sources.len(), expected.len());
    let available = NativeCatalogue {
        format_version: 1,
        repository: "test".into(),
        sequence: 1,
        packages: vec![],
    };
    for source in &extension.sources {
        assert_eq!(source.id, expected[source.language.as_str()]);
        assert_eq!(source.home_url, "https://mangafire.to");
        let mapping = mappings
            .iter()
            .find(|m| {
                m.repository == KEIYOUSHI_INDEX
                    && m.package_name == extension.package_name
                    && m.source_id == source.id
            })
            .unwrap();
        assert_eq!(mapping.native_addon_id, "mangafire");
        assert_eq!(
            mapping.native_source_id,
            format!("mangafire.{}", source.language)
        );
        assert!(matches!(
            upstream_state(
                KEIYOUSHI_INDEX,
                &extension.package_name,
                &source.id,
                &mappings,
                &available,
                &BTreeMap::new()
            ),
            UpstreamState::NotYetPorted { .. }
        ));
    }
    let json = serde_json::to_value(catalogue).unwrap();
    assert!(
        json["extensions"][0]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"].is_string())
    );
}
