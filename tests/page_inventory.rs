use nova_reader::{
    page_inventory::{self, ResourceKind},
    services::Services,
};
use reader_domain::ChapterId;
use reader_sources::CancelToken;

#[test]
fn local_inventory_is_metadata_only_and_original_integrity_is_checked_when_selected() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::open(&directory.path().join("app")).unwrap();
    let input = directory.path().join("collection");
    std::fs::create_dir(&input).unwrap();
    for n in 0..3 {
        image::RgbaImage::from_pixel(12, 18, image::Rgba([20 + n, 40, 60, 255]))
            .save(input.join(format!("{n}.png")))
            .unwrap();
    }
    let series = services.import_local(&input, true).unwrap();
    let chapter = ChapterId::new(series, "chapter");
    let before = services.domain.backup_json().unwrap();
    let cancel = CancelToken::default();
    let inventory = page_inventory::load(&services, &chapter, &cancel).unwrap();
    assert_eq!(inventory.pages.len(), 3);
    let ids = inventory
        .pages
        .iter()
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    assert!(
        inventory
            .pages
            .iter()
            .all(|p| matches!(p.kind, ResourceKind::Local { .. }))
    );
    let ResourceKind::Local { path, .. } = &inventory.pages[1].kind else {
        panic!()
    };
    std::fs::write(path, b"damaged page").unwrap();
    // Opening a grid does not hash/drop every page or repair its manifest.
    let reopened = page_inventory::load(&services, &chapter, &cancel).unwrap();
    assert_eq!(
        reopened
            .pages
            .iter()
            .map(|p| p.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(reopened.pages[0].read(&cancel).is_ok());
    assert!(
        reopened.pages[1]
            .read(&cancel)
            .unwrap_err()
            .contains("unavailable offline")
    );
    assert!(reopened.pages[2].read(&cancel).is_ok());
    assert_eq!(services.domain.backup_json().unwrap(), before);
    assert!(services.domain.snapshot().unwrap().history.is_empty());
    assert!(services.domain.snapshot().unwrap().progress.is_empty());
    cancel.cancel();
    assert!(page_inventory::load(&services, &chapter, &cancel).is_err());
}
