//! Identity of user-created library categories.

/// A new category id, unique to this device: the device id plus a local
/// sequence. The sequence starts at 1 the first time a category is created.
pub fn new_id() -> Result<String, String> {
    Ok(format!(
        "category-{:016x}-{}",
        nova_sync::local_device().map_err(|e| e.to_string())?,
        nova_storage::try_allocate_local_sequence("reader:category", 1)
            .map_err(|e| e.to_string())?
    ))
}
