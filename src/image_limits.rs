//! Device-local image admission policy; never synchronized between devices.
use nova_reader_ui::backend_text as text;
use serde::{Deserialize, Serialize};

const KEY: &str = "reader:image-limits";
static MUTATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub encoded_mib: u32,
    pub width: u32,
    pub height: u32,
    pub megapixels: u32,
    pub decoded_mib: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            encoded_mib: 32,
            width: 16384,
            height: 65536,
            megapixels: 32,
            decoded_mib: 128,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        for (valid, message) in [
            (
                (1..=1024).contains(&self.encoded_mib),
                "Encoded image limit must be between 1 and 1024 MiB",
            ),
            (
                (1..=1_048_576).contains(&self.width),
                "Image width limit must be between 1 and 1048576 pixels",
            ),
            (
                (1..=1_048_576).contains(&self.height),
                "Image height limit must be between 1 and 1048576 pixels",
            ),
            (
                (1..=1024).contains(&self.megapixels),
                "Image pixel limit must be between 1 and 1024 megapixels",
            ),
            (
                (1..=2048).contains(&self.decoded_mib),
                "Decoded image limit must be between 1 and 2048 MiB",
            ),
        ] {
            if !valid {
                return Err(text::tr(message));
            }
        }
        Ok(())
    }
    pub fn decode_limits(&self) -> reader_engine::DecodeLimits {
        reader_engine::DecodeLimits {
            max_encoded_bytes: self.encoded_mib as usize * 1024 * 1024,
            max_width: self.width,
            max_height: self.height,
            max_pixels: u64::from(self.megapixels) * 1_000_000,
            max_decoded_bytes: u64::from(self.decoded_mib) * 1024 * 1024,
        }
    }
    /// Invalid/unavailable persisted policy falls back without overwriting it.
    pub fn load() -> Self {
        nova_storage::try_get_str(KEY)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Self>(&json).ok())
            .filter(|settings| settings.validate().is_ok())
            .unwrap_or_default()
    }
    /// Publish only after the durable write succeeds. Existing files are untouched.
    pub fn save(&self) -> Result<(), String> {
        self.validate()?;
        let _mutation = MUTATION.lock().unwrap_or_else(|error| error.into_inner());
        let json = serde_json::to_string(self).map_err(|error| error.to_string())?;
        nova_storage::try_set_str(KEY, &json).map_err(|error| error.to_string())?;
        reader_engine::set_limits(self.decode_limits());
        Ok(())
    }
    pub fn reset() -> Result<(), String> {
        Self::default().save()
    }
}
/// Call after storage initialization, before starting worker services.
pub fn install() {
    let _mutation = MUTATION.lock().unwrap_or_else(|error| error.into_inner());
    reader_engine::set_limits(Settings::load().decode_limits());
}
