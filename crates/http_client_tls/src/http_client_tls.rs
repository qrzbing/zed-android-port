use std::sync::OnceLock;

use rustls::{ClientConfig, RootCertStore};

static TLS_CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();

/// Build (once) a rustls ClientConfig that trusts the Mozilla CA bundle via
/// `webpki-roots`. We previously used `rustls-platform-verifier`, but on
/// Android its TrustManager bridge maps `UNDETERMINED_REVOCATION_STATUS` to
/// `CertificateError::Revoked` — which makes downloads from CDN hosts like
/// `release-assets.githubusercontent.com` (Let's Encrypt cert, no OCSP
/// responder) fail with a spurious "Revoked" error. Static roots + no
/// revocation matches the de-facto behavior of reqwest's default
/// `rustls-tls-webpki-roots` and avoids that whole class.
pub fn tls_config() -> ClientConfig {
    TLS_CONFIG
        .get_or_init(|| {
            // rustls uses the `aws_lc_rs` provider by default. install_default
            // errors if a provider is already installed; ignore that case.
            rustls::crypto::aws_lc_rs::default_provider()
                .install_default()
                .ok();

            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        })
        .clone()
}
