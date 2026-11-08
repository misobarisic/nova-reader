//! Metadata backups from before archives existed: a pretty-printed
//! `ReaderState` JSON of one profile. They open as a single-profile archive
//! that restores with Replace into the profile on screen, which is what
//! restoring them always did, except that the installed-addon list in them is
//! dropped: it names the packages of the device the backup came from.
use super::{
    archive::{Archive, Component, Manifest, Payload, Profile, digest},
    components,
    plan::Opened,
};
use std::{collections::BTreeMap, io::Read, path::Path};

const LIMIT: u64 = 32 * 1024 * 1024;
const ARCHIVE_PROFILE: &str = "p0";
const COMPONENTS: &[&str] = &["library", "categories", "progress", "history", "bookmarks"];

pub(super) fn open(path: &Path) -> Result<Opened, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > LIMIT {
        return Err("Backup exceeds 32 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Backup exceeds 32 MiB".into());
    }
    let archive = from_json(&bytes)?;
    Ok(Opened {
        archive,
        path: path.to_owned(),
        temporary: false,
        legacy: true,
    })
}

/// Wrap a metadata backup as the archive of the selected profile.
pub(crate) fn from_json(bytes: &[u8]) -> Result<Archive, String> {
    let state = reader_domain::parse_backup(bytes).map_err(|e| e.to_string())?;
    let selected = reader_domain::profile::selected();
    let name = crate::profiles::list()?
        .into_iter()
        .find(|profile| profile.id == selected)
        .map_or_else(|| "Profile".to_owned(), |profile| profile.name);
    let mut payloads = BTreeMap::new();
    let mut described = Vec::new();
    for name in COMPONENTS {
        let payload: Payload = components::capture_state(name, &state)?;
        described.push(Component {
            name: (*name).to_owned(),
            schema_version: 1,
            path: format!("profiles/{ARCHIVE_PROFILE}/{name}.json"),
            item_count: payload.records.len(),
            sha256: digest(&serde_json::to_vec(&payload).map_err(|e| e.to_string())?),
        });
        // The same checks an archive component passes on decode.
        components::validate(name, &payload)?;
        payloads.insert((ARCHIVE_PROFILE.to_owned(), (*name).to_owned()), payload);
    }
    Ok(Archive {
        manifest: Manifest {
            format: super::archive::FORMAT.into(),
            version: 1,
            created_at: 0,
            app_version: "metadata backup".into(),
            archive_id: format!("metadata-{}", &digest(bytes)[..16]),
            profiles: vec![Profile {
                archive_profile_id: ARCHIVE_PROFILE.into(),
                source_profile_id: selected,
                name,
                components: described,
            }],
            packages: Vec::new(),
        },
        payloads,
        checksum: digest(bytes),
        unsupported: Vec::new(),
        undo_after: None,
        packages: BTreeMap::new(),
        undo_local: None,
    })
}
