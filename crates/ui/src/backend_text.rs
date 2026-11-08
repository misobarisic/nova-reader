//! Central English backend messages. UI literals use Slint's translation API;
//! new languages can provide these formatted messages alongside their catalog.
pub fn tr(message: &str) -> String {
    message.to_owned()
}
pub fn download_progress(chapters: usize, completed: usize, total: usize) -> String {
    let label = if chapters == 1 { "chapter" } else { "chapters" };
    format!("{chapters} {label} · {completed} / {total} pages")
}
pub fn chapter_count(unread: usize, total: usize) -> String {
    format!("{unread} unread · {total} chapters")
}
pub fn image_cache_usage(bytes: u64, files: usize) -> String {
    format!(
        "{:.1} MiB · {files} cached images",
        bytes as f64 / (1024.0 * 1024.0)
    )
}
/// A file size for save summaries: KiB below one MiB, MiB with one decimal above.
pub fn file_size(bytes: usize) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if (bytes as f64) < MIB {
        format!("{} KiB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB)
    }
}
pub fn image_cache_clear_failed(error: &str) -> String {
    format!("{}: {error}", tr("Could not clear image cache"))
}
/// Count-aware nouns. Pieces like `{n} {"chapters"}` read "1 chapters", so
/// callers pass the number and get the whole phrase.
pub fn chapters_label(count: usize) -> String {
    if count == 1 {
        "1 chapter".to_owned()
    } else {
        format!("{count} chapters")
    }
}
pub fn pages_label(count: usize) -> String {
    if count == 1 {
        "1 page".to_owned()
    } else {
        format!("{count} pages")
    }
}
pub fn titles_label(count: usize) -> String {
    if count == 1 {
        "1 title".to_owned()
    } else {
        format!("{count} titles")
    }
}
/// Plural-aware elapsed-time phrases; callers pass the number, not a unit word.
pub fn minutes_ago(count: u64) -> String {
    if count == 1 {
        "1 minute ago".to_owned()
    } else {
        format!("{count} minutes ago")
    }
}
pub fn hours_ago(count: u64) -> String {
    if count == 1 {
        "1 hour ago".to_owned()
    } else {
        format!("{count} hours ago")
    }
}
pub fn days_ago(count: u64) -> String {
    if count == 1 {
        "1 day ago".to_owned()
    } else {
        format!("{count} days ago")
    }
}
pub fn update_available(version: &str) -> String {
    format!("Nova Reader {version} is available at github.com/misobarisic/nova-reader/releases")
}
/// Android pickers and the QR scanner report a user backing out as an error
/// string. That is a choice, not a failure, so callers stay silent for it.
pub fn is_user_cancel(raw: &str) -> bool {
    matches!(
        raw,
        "Document selection cancelled" | "Image action cancelled" | "QR scan cancelled"
    )
}
/// Raw lower-layer failures (sync, storage, platform) are shown through the
/// same translation entry point as every other backend message.
pub fn error(raw: &str) -> String {
    tr(raw)
}
/// Store errors are developer wording; show readers an actionable sentence.
/// Unknown messages pass through unchanged.
pub fn download_error(raw: &str) -> String {
    if raw.starts_with("chapter already queued") {
        tr("This chapter is already in the download queue")
    } else if raw.contains("download quota exceeded") {
        tr("Not enough download storage. Delete some downloads and try again.")
    } else if raw.starts_with("chapter already running") {
        tr("This chapter is already downloading")
    } else if raw.contains("not fully available offline") {
        tr("This chapter is not fully downloaded yet")
    } else {
        tr(raw)
    }
}
/// Status shown after "Refresh library" when some sources could not answer.
pub fn library_refresh_failed(failed: usize) -> String {
    let label = if failed == 1 {
        "source refresh"
    } else {
        "source refreshes"
    };
    format!("{failed} {label} failed. Saved chapters remain available.")
}
/// Source picker suffix for an addon the user switched off.
pub fn disabled_suffix() -> String {
    tr(" (disabled)")
}
/// Confirmation body for removing downloaded chapters from this device.
pub fn download_delete_prompt(count: usize, size: &str) -> String {
    format!(
        "{} ({size}). {}",
        chapters_label(count),
        tr("Deleted downloads are removed from this device; download again to read offline.")
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_are_singular_for_one() {
        assert_eq!(chapters_label(1), "1 chapter");
        assert_eq!(pages_label(1), "1 page");
        assert_eq!(titles_label(1), "1 title");
        assert_eq!(pages_label(0), "0 pages");
        assert_eq!(chapters_label(2), "2 chapters");
    }
    #[test]
    fn user_cancellation_is_not_an_error() {
        assert!(is_user_cancel("Document selection cancelled"));
        assert!(is_user_cancel("QR scan cancelled"));
        assert!(!is_user_cancel("Android returned no document path"));
    }
    #[test]
    fn elapsed_time_is_singular_for_one() {
        assert_eq!(minutes_ago(1), "1 minute ago");
        assert_eq!(hours_ago(1), "1 hour ago");
        assert_eq!(days_ago(1), "1 day ago");
        assert_eq!(hours_ago(5), "5 hours ago");
    }
    #[test]
    fn raw_store_errors_are_mapped() {
        assert!(
            !download_error("chapter already queued; retry existing job or cancel first")
                .contains(';')
        );
        assert_eq!(
            download_error("download quota exceeded"),
            "Not enough download storage. Delete some downloads and try again."
        );
        assert_eq!(download_error("HTTP 503"), "HTTP 503");
    }
}
