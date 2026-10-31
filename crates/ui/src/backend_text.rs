//! Central English backend messages. UI literals use Slint's translation API;
//! new languages can provide these formatted messages alongside their catalog.
pub fn tr(message: &str) -> String {
    message.to_owned()
}
pub fn page_position(page: usize, count: usize) -> String {
    format!("{} / {count}", page + 1)
}
pub fn chapter_count(unread: usize, total: usize) -> String {
    format!("{unread} unread · {total} chapters")
}
