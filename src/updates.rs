use nova_reader_ui::backend_text as text;

#[cfg(feature = "update-check")]
pub fn check() -> Result<String, String> {
    use std::io::Read;
    let builder = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Nova-Reader");
    #[cfg(target_os = "android")]
    let builder = builder.tls_backend_preconfigured(
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::from_iter(
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
            ))
            .with_no_client_auth(),
    );
    let client = builder.build().map_err(|e| text::error(&e.to_string()))?;
    let mut response = client
        .get("https://api.github.com/repos/misobarisic/nova-reader/releases/latest")
        .send()
        .map_err(|_| text::tr("Update check failed. Try again later."))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(text::tr("No releases have been published yet."));
    }
    if !response.status().is_success() {
        return Err(text::tr("Release service is unavailable. Try again later."));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| text::error(&e.to_string()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(text::tr("Release metadata exceeds its limit."));
    }
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
