//! SELinux canary kept after Phase 8b of the Termux-divestment refactor.
//!
//! Phase 6 moved bootstrap-zip extraction into `BootstrapAdapter::install`
//! (in `zdroid_runtime`). Phase 8a deleted the APK-asset extraction path.
//! Phase 8b moved every `install_*` runtime hook into the bootstrap zip
//! itself — the `Dylanmurzello/zdroid-bootstrap` repo ships them under
//! `patches/` and the rebuilt zip lands them at extraction time. All that
//! remains here is the targetSdk SELinux-domain canary, which guards the
//! one constraint we can't bake into the bootstrap zip.

/// Logs the process's SELinux context. If `targetSdk >= 29` ever sneaks
/// back into `build.gradle.kts`, the JVM lands in `untrusted_app_all`
/// where `execute_no_trans` on `app_data_file` is denied — every spawned
/// binary fails with `EACCES`. Catching it loudly here is faster than
/// bisecting through "why does bash crash".
pub fn check_selinux_context() {
    let context = std::fs::read_to_string("/proc/self/attr/current").ok();
    log::info!("termux_bootstrap: /proc/self/attr/current = {:?}", context);
    let Some(c) = context.as_deref() else {
        return;
    };
    if !c.contains("untrusted_app_27") && !c.contains("untrusted_app_25") {
        log::error!(
            "termux_bootstrap: SELinux domain {} disallows execute_no_trans on \
             app_data_file. Verify build.gradle.kts pins targetSdk=28; otherwise \
             every $PREFIX/bin/* spawn will EACCES.",
            c.trim()
        );
    }
}
