//! Central English backend messages. UI literals use Slint's translation API;
//! new languages can provide these formatted messages alongside their catalog.
pub fn tr(message: &str) -> String {
    message.to_owned()
}
pub fn page_position(page: usize, count: usize) -> String {
    format!("{} / {count}", page + 1)
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
pub fn image_cache_clear_failed(error: &str) -> String {
    format!("{}: {error}", tr("Could not clear image cache"))
}
