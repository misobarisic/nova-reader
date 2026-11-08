//! Category ids on a device that has never created one.

#[test]
fn the_first_category_on_a_fresh_device_gets_an_id() {
    let storage = tempfile::tempdir().unwrap();
    nova_storage::init_at(storage.path());
    let first = nova_reader::categories::new_id().expect("the first category needs an id");
    let second = nova_reader::categories::new_id().unwrap();
    assert!(
        first.starts_with("category-") && first.ends_with("-1"),
        "{first}"
    );
    assert!(second.ends_with("-2"), "{second}");
}
