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
    let client = builder.build().map_err(|e| e.to_string())?;
    let mut response = client
        .get("https://api.github.com/repos/misobarisic/nova-reader/releases/latest")
        .send()
        .map_err(|_| "Update check failed. Try again later.".to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok("No releases have been published yet.".into());
    }
    if !response.status().is_success() {
        return Err("Release service is unavailable. Try again later.".into());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Release metadata exceeds its limit.".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("Release version is missing")?;
    let version = semver::Version::parse(tag.trim_start_matches('v')).map_err(|e| e.to_string())?;
    if version > semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap() {
        Ok(format!(
            "Nova Reader {version} is available at github.com/misobarisic/nova-reader/releases"
        ))
    } else {
        Ok("You are using the latest release.".into())
    }
}
#[cfg(not(feature = "update-check"))]
pub fn check() -> Result<String, String> {
    Ok("Update checking is disabled in this build.".into())
}
