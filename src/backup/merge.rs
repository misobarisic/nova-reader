//! Record-level merge of a backup component into the live one, the counts the
//! preview shows, and the compensating merge an undo uses.
use super::{
    archive::Payload,
    components::{self, is_support},
};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub archived: usize,
    pub added: usize,
    pub identical: usize,
    pub conflicts: usize,
    pub updated: usize,
    pub preserved: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub examples: Vec<String>,
}

fn last_read(value: &Value) -> i64 {
    value
        .get("last_read_at")
        .or_else(|| value.get("read_at"))
        .and_then(Value::as_i64)
        .unwrap_or(i64::MIN)
}

/// Which side wins when both have the record and they differ.
fn conflict(name: &str, local: &Value, backup: &Value, use_backup: bool) -> Value {
    if use_backup {
        return backup.clone();
    }
    match name {
        // The same rule as sync: the more recent reading activity wins, and the
        // device's own record on a tie.
        "progress" | "history" if last_read(backup) > last_read(local) => backup.clone(),
        _ => local.clone(),
    }
}

/// The records a restore leaves in a component. Merge adds what is missing and
/// keeps conflicting local values (newer reading activity wins for progress and
/// history), unless `use_backup` takes the archived value of every conflict.
/// Replace makes the backup's records the component's records and deletes the
/// rest. Files are only ever added: neither option replaces or removes one.
/// Cached titles and chapters are inserted when missing and otherwise untouched.
pub(crate) fn merge(
    name: &str,
    local: &Payload,
    backup: &Payload,
    replace: bool,
    use_backup: bool,
) -> Payload {
    let files = components::is_files(name);
    let replace = replace && !files;
    let use_backup = use_backup && !files;
    let mut result = if replace {
        // Support records are a cache; deleting them would orphan nothing, but
        // it would also discard titles the device may need elsewhere.
        Payload {
            version: 1,
            records: local
                .records
                .iter()
                .filter(|(key, _)| is_support(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    } else {
        local.clone()
    };
    for (key, value) in &backup.records {
        match local.records.get(key) {
            None => {
                result.records.insert(key.clone(), value.clone());
            }
            Some(_) if is_support(key) => {}
            // A category is its name as far as matching goes; a restore never
            // renames one that exists, so spelling differences are no conflict.
            Some(old) if name == "categories" => {
                result.records.insert(key.clone(), old.clone());
            }
            Some(old) if old == value => {
                result.records.insert(key.clone(), value.clone());
            }
            Some(old) => {
                let winner = if replace && !is_support(key) {
                    // Replace has no local side to keep.
                    value.clone()
                } else {
                    conflict(name, old, value, use_backup)
                };
                result.records.insert(key.clone(), winner);
            }
        }
    }
    result
}

/// Revert only the records that still equal what the import wrote: a record the
/// user or a peer edited since is a conflict and stays, unless `use_backup`.
pub(crate) fn undo_merge(
    current: &Payload,
    before: &Payload,
    imported: &Payload,
    use_backup: bool,
) -> Payload {
    let mut restored = current.clone();
    for key in before
        .records
        .keys()
        .chain(imported.records.keys())
        .collect::<BTreeSet<_>>()
    {
        if is_support(key) {
            continue;
        }
        if use_backup || current.records.get(key) == imported.records.get(key) {
            match before.records.get(key) {
                Some(value) => {
                    restored.records.insert(key.clone(), value.clone());
                }
                None => {
                    restored.records.remove(key);
                }
            }
        }
    }
    restored
}

/// What a restore did to `local`, as counts and a few examples.
pub(crate) fn count(name: &str, local: &Payload, backup: &Payload, result: &Payload) -> Counts {
    let mut counts = Counts::default();
    for (key, value) in &backup.records {
        if is_support(key) {
            continue;
        }
        counts.archived += 1;
        if !result.records.contains_key(key) {
            counts.skipped += 1;
            continue;
        }
        match local.records.get(key) {
            None => counts.added += 1,
            Some(old) if old == value || name == "categories" => counts.identical += 1,
            Some(_) => {
                counts.conflicts += 1;
                if counts.examples.len() < 3 {
                    counts.examples.push(components::describe(key));
                }
            }
        }
    }
    for (key, old) in &local.records {
        if is_support(key) {
            continue;
        }
        match result.records.get(key) {
            None => counts.deleted += 1,
            Some(new) if new != old => counts.updated += 1,
            _ => counts.preserved += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(rows: &[(&str, Value)]) -> Payload {
        Payload {
            version: 1,
            records: rows
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
        }
    }

    #[test]
    fn merge_keeps_local_conflicts_and_replace_takes_the_archive() {
        let local = payload(&[
            ("bookmark\0a", json!({"note": "mine"})),
            ("bookmark\0b", json!({"note": "only here"})),
            ("series\0s", json!({"title": "cached here"})),
        ]);
        let backup = payload(&[
            ("bookmark\0a", json!({"note": "theirs"})),
            ("bookmark\0c", json!({"note": "new"})),
            ("series\0s", json!({"title": "cached there"})),
        ]);
        let merged = merge("bookmarks", &local, &backup, false, false);
        assert_eq!(merged.records["bookmark\0a"], json!({"note": "mine"}));
        assert!(merged.records.contains_key("bookmark\0b"));
        assert!(merged.records.contains_key("bookmark\0c"));
        let preferred = merge("bookmarks", &local, &backup, false, true);
        assert_eq!(preferred.records["bookmark\0a"], json!({"note": "theirs"}));
        assert!(preferred.records.contains_key("bookmark\0b"));
        // Cached titles are never replaced by an archive's copy.
        assert_eq!(
            preferred.records["series\0s"],
            json!({"title": "cached here"})
        );
        let replaced = merge("bookmarks", &local, &backup, true, false);
        assert_eq!(replaced.records["bookmark\0a"], json!({"note": "theirs"}));
        assert!(!replaced.records.contains_key("bookmark\0b"));
        assert!(replaced.records.contains_key("series\0s"));
        let counts = count("bookmarks", &local, &backup, &replaced);
        assert_eq!(
            (
                counts.archived,
                counts.added,
                counts.conflicts,
                counts.deleted
            ),
            (2, 1, 1, 1)
        );
        assert_eq!(counts.examples.len(), 1);
    }

    #[test]
    fn newer_reading_activity_wins_a_progress_conflict() {
        let local = payload(&[
            (
                "progress\0old",
                json!({"last_read_at": 10, "page": "local"}),
            ),
            (
                "progress\0new",
                json!({"last_read_at": 30, "page": "local"}),
            ),
            (
                "progress\0tie",
                json!({"last_read_at": 20, "page": "local"}),
            ),
        ]);
        let backup = payload(&[
            (
                "progress\0old",
                json!({"last_read_at": 20, "page": "backup"}),
            ),
            (
                "progress\0new",
                json!({"last_read_at": 25, "page": "backup"}),
            ),
            (
                "progress\0tie",
                json!({"last_read_at": 20, "page": "backup"}),
            ),
        ]);
        let merged = merge("progress", &local, &backup, false, false);
        assert_eq!(merged.records["progress\0old"]["page"], "backup");
        assert_eq!(merged.records["progress\0new"]["page"], "local");
        assert_eq!(merged.records["progress\0tie"]["page"], "local");
        // An explicit choice of the archive beats the clock.
        let preferred = merge("progress", &local, &backup, false, true);
        assert_eq!(preferred.records["progress\0new"]["page"], "backup");
        // Other components have no clock: local wins.
        let other = merge("bookmarks", &local, &backup, false, false);
        assert_eq!(other.records["progress\0old"]["page"], "local");
    }

    #[test]
    fn files_are_added_but_never_replaced_or_deleted() {
        let local = payload(&[
            ("package\0a", json!({"v": 1})),
            ("package\0b", json!({"v": 1})),
        ]);
        let backup = payload(&[
            ("package\0a", json!({"v": 2})),
            ("package\0c", json!({"v": 2})),
        ]);
        let merged = merge("packages", &local, &backup, true, true);
        assert_eq!(merged.records["package\0a"], json!({"v": 1}));
        assert!(merged.records.contains_key("package\0b"));
        assert!(merged.records.contains_key("package\0c"));
    }

    #[test]
    fn undo_reverts_only_what_still_equals_the_import() {
        let before = payload(&[("bookmark\0kept", json!(1)), ("bookmark\0edited", json!(1))]);
        let imported = payload(&[
            ("bookmark\0kept", json!(2)),
            ("bookmark\0edited", json!(2)),
            ("bookmark\0added", json!(2)),
        ]);
        let current = payload(&[
            ("bookmark\0kept", json!(2)),
            ("bookmark\0edited", json!(3)),
            ("bookmark\0added", json!(2)),
            ("bookmark\0later", json!(9)),
        ]);
        let undone = undo_merge(&current, &before, &imported, false);
        assert_eq!(undone.records["bookmark\0kept"], json!(1));
        assert_eq!(undone.records["bookmark\0edited"], json!(3));
        assert!(!undone.records.contains_key("bookmark\0added"));
        assert_eq!(undone.records["bookmark\0later"], json!(9));
        let forced = undo_merge(&current, &before, &imported, true);
        assert_eq!(forced.records["bookmark\0edited"], json!(1));
        assert_eq!(forced.records["bookmark\0later"], json!(9));
    }
}
