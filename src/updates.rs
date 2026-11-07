use nova_reader_ui::backend_text as text;

#[cfg(feature = "update-check")]
pub fn check() -> Result<String, String> {
    let builder = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Nova-Reader");
    #[cfg(target_os = "android")]
    let builder = builder.tls_backend_preconfigured(
        nova_net::tls::client_config().map_err(|e| text::error(&e.to_string()))?,
    );
    let client = builder.build().map_err(|e| text::error(&e.to_string()))?;
    let response = client
        .get("https://api.github.com/repos/misobarisic/nova-reader/releases/latest")
        .send()
        .map_err(|_| text::tr("Update check failed. Try again later."))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(text::tr("No releases have been published yet."));
    }
    if !response.status().is_success() {
        return Err(text::tr("Release service is unavailable. Try again later."));
    }
    let bytes = nova_net::read_capped(response, 1024 * 1024).map_err(|e| match e {
        nova_net::ReadError::TooLarge => text::tr("Release metadata exceeds its limit."),
        nova_net::ReadError::Io(e) => text::error(&e.to_string()),
    })?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| text::error(&e.to_string()))?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| text::tr("Release version is missing"))?;
    let version = semver::Version::parse(tag.trim_start_matches('v'))
        .map_err(|e| text::error(&e.to_string()))?;
    if version > semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap() {
        Ok(text::update_available(&version.to_string()))
    } else {
        Ok(text::tr("You are using the latest release."))
    }
}
#[cfg(not(feature = "update-check"))]
pub fn check() -> Result<String, String> {
    Ok(text::tr("Update checking is disabled in this build."))
}
