# SSL_CERT_FILE / CURL_CA_BUNDLE env

**Status:** Removed — Phase 1 of Termux-divestment refactor.

Zed-the-Rust-process no longer touches these env vars at boot. All in-
process HTTPS goes through `http_client_tls` (rustls + `rustls-platform-
verifier-android`, which queries Android's native trust store via JNI)
or `reqwest` with `rustls-tls-native-roots` (queries the same store via
`rustls-native-certs`). The dep tree for `zed_android` has zero
`openssl-sys` / `native-tls` / `curl-sys` consumers, so no in-process
client needs a CA bundle path.

Subprocess CA story is adapter-owned:

- **Chroot adapter** — `sanitize_env_for_chroot` excludes the var. The
  rootfs ships `/etc/ssl/certs` (Debian-style), self-sufficient.
- **External Termux adapter** — spawns via Intent; the receiver lives
  in Termux's own process with Termux's env, self-sufficient.
- **Bootstrap adapter** — currently inherits caller env. Subsumed by
  Phase 3 (adapter-owned env contract). Until then, integrated terminal
  spawns still get the bundle via `terminal.rs::185-195`'s PREFIX-based
  setup (a Phase 5 target).
