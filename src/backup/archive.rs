//! Reader's binding of the shared backup container (kit `nova-backup`): the
//! format, limits and strict decoding live in the kit, while which components
//! exist, how their records are validated and how a bundled addon package is
//! checked are decided here. Applying and undoing a restore live in `apply.rs`.
use super::components;
#[cfg(test)]
pub(super) use nova_backup::archive::encode_with_packages;
pub(super) use nova_backup::archive::{
    Archive, BLOB_PREFIX, Component, Manifest, Package, Payload, Profile, Writer, digest,
};
use nova_backup::archive::{PackageIdentity, Schema};
use std::{fs::File, io::BufReader, path::Path};

pub const FORMAT: &str = "nova-reader-backup";
/// The `archive_profile_id` of the entry that holds device-wide components.
/// It is not a profile: it has no source profile and is never mapped.
pub const DEVICE: &str = "device";

struct ReaderSchema;

impl Schema for ReaderSchema {
    fn format(&self) -> &str {
        FORMAT
    }
    fn supported(&self, component: &str) -> bool {
        components::supported(component)
    }
    fn validate(&self, component: &str, payload: &Payload) -> Result<(), String> {
        components::validate(component, payload)
    }
    fn max_package_bytes(&self) -> u64 {
        nova_sync::MAX_PACKAGE_BYTES as u64
    }
    fn package(&self, bytes: &[u8]) -> Result<PackageIdentity, String> {
        let (descriptor, sha256) =
            reader_sources::SourceRegistry::inspect_archive(bytes).map_err(|e| e.to_string())?;
        Ok(PackageIdentity {
            id: descriptor.id,
            version: descriptor.version,
            sha256,
        })
    }
    fn declared_packages(&self, component: &str, payload: &Payload) -> Vec<Option<String>> {
        if component != "packages" {
            return Vec::new();
        }
        payload
            .records
            .values()
            .map(|value| value["sha256"].as_str().map(str::to_owned))
            .collect()
    }
}

/// Read and fully validate an archive file. Nothing in it is applied.
pub(super) fn decode_file(path: &Path) -> Result<Archive, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    nova_backup::archive::decode_reader(BufReader::new(file), &ReaderSchema)
}

/// Whether the file starts with the ZIP signature. Anything else is treated as
/// a legacy JSON metadata backup.
pub(super) fn is_zip(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    let mut magic = [0u8; 2];
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    Ok(file.read_exact(&mut magic).is_ok() && &magic == b"PK")
}
