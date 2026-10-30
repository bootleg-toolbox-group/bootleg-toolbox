//! Looks for a newer release on GitHub, only when the user asks for it (Settings option or
//! Help menu). One HTTPS request to github.com, which sends nothing but the program's name
//! and version: the "latest release" page redirects to the release's tag, which gives the
//! version, with no account, token or API rate limit involved.

use std::time::Duration;

pub const RELEASES: &str = "https://github.com/bootleg-toolbox-group/bootleg-toolbox/releases";

/// The program's HTTPS client: rustls with the ring cryptography, the certificates checked
/// by the system (Windows certificate store, or the distribution's certificates on Linux),
/// no redirects (nothing sent to another server than the one asked), HTTP errors as answers.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .user_agent(format!("bootleg-toolbox/{}", crate::ops::APP_VERSION))
        .tls_config(tls)
        .build()
        .into()
}

/// A newer version than this one, if there is one ("0.1.5").
pub fn newer_version() -> Result<Option<String>, String> {
    let agent = agent(Duration::from_secs(15));
    // BT_RELEASES: another releases page, for tests.
    let releases = std::env::var("BT_RELEASES").unwrap_or_else(|_| RELEASES.to_string());
    let response = agent.get(&format!("{releases}/latest")).call().map_err(|e| format!("cannot reach github.com ({e})"))?;
    match response.status().as_u16() {
        300..=399 => {}
        404 => return Err("no published release found (is the repository public?)".into()),
        code => return Err(format!("github.com answered {code}")),
    }
    // ".../releases/tag/v0.1.5"
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or("unexpected answer from github.com")?;
    let tag = location.rsplit('/').next().unwrap_or("");
    let latest = tag.trim_start_matches('v');
    let parse = |v: &str| -> Option<Vec<u32>> { v.split('.').map(|n| n.parse().ok()).collect() };
    let (Some(theirs), Some(ours)) = (parse(latest), parse(crate::ops::APP_VERSION)) else {
        return Err(format!("unexpected release name \"{tag}\""));
    };
    Ok((theirs > ours).then(|| latest.to_string()))
}
