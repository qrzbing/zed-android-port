# SSL_CERT_FILE / CURL_CA_BUNDLE env

**Status:** Active

Termux ships pre-populated CA bundle at $PREFIX/etc/tls/cert.pem. cargo (libcurl-vendored), npm, curl, rustls-via-openssl-rs all honor SSL_CERT_FILE; older curl honors CURL_CA_BUNDLE. We set both at boot. Without this, cargo metadata fails on first crates.io fetch with 'unable to get local issuer certificate'.

**Detailed writeup: TODO.** Stub created so the index links resolve.
