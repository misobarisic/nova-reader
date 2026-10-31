use std::path::PathBuf;

#[cfg(target_os = "android")]
pub static ANDROID_DATA: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn data_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("NOVA_READER_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "android")]
    return ANDROID_DATA
        .get()
        .cloned()
        .ok_or_else(|| "Android did not provide app-private storage".into());
    #[cfg(not(target_os = "android"))]
    directories::ProjectDirs::from("dev", "misobarisic", "nova-reader")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .ok_or_else(|| "Application data directory is unavailable".into())
}
