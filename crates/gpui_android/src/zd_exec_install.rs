//! Install / refresh the `zd-exec` binary from APK assets to
//! `$PREFIX/bin/zd-exec`.
//!
//! `zd-exec` is the Rust spawn wrapper that routes every PATH-resolved
//! invocation (bash, git, rust-analyzer, …) through the active
//! `RuntimeProvider` (chroot / bootstrap / external Termux). The APK
//! ships it as an asset (`android/app/src/main/assets/zd-exec`) and
//! we extract it to `$PREFIX/bin/zd-exec` at boot. The Gradle
//! `buildZdExec` task in `app/build.gradle.kts` produces the asset by
//! running `cargo ndk … build --release -p zdroid_runtime --bin
//! zd-exec` before each APK build, so the binary stays in lockstep
//! with the Rust libs inside the APK.
//!
//! Why APK-bundled instead of part of the bootstrap zip: zd-exec is
//! tied to the editor's Rust code (RuntimeProvider trait, wire
//! protocol with zd-spawnd, etc.) — bumping the APK with a new
//! adapter or protocol revision must always bring a matching zd-exec.
//! The Termux bootstrap zip is independently versioned for the
//! userland (libc, coreutils, package set) and shouldn't drag in our
//! Rust binaries. Two artifacts, two cadences.
//!
//! Idempotent: a quick byte-length comparison decides whether to
//! re-extract. Fast enough on flash to do on every boot if the
//! comparison detects a stale binary.

use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use android_activity::AndroidApp;
use anyhow::{Context, Result, anyhow};

/// Asset file name inside the APK. Matches the staging path the
/// `buildZdExec` Gradle task writes to in `assets/`.
const ASSET_NAME: &str = "zd-exec";

/// Ensure `<prefix>/bin/zd-exec` exists and matches the APK-bundled
/// asset. Re-extracts if the file is missing or has a different byte
/// length than the asset (catches both fresh-install and stale-binary
/// cases). No-op when the on-disk file already matches.
///
/// `prefix` is typically `<app data>/files/usr`, mirroring the
/// Termux-flavored layout we extract into.
pub fn ensure_installed(android_app: &AndroidApp, prefix: &Path) -> Result<()> {
    let target = prefix.join("bin").join(ASSET_NAME);

    let asset_manager = android_app.asset_manager();
    let asset_name = CString::new(ASSET_NAME)?;
    let mut asset = asset_manager
        .open(&asset_name)
        .ok_or_else(|| anyhow!("{ASSET_NAME} asset not present in APK; check `buildZdExec` Gradle task ran"))?;
    let expected_len = asset.length();

    // Skip re-extraction when the destination already matches the
    // asset's byte length. Won't catch silent corruption (mismatched
    // content with matching size), but that's exceedingly rare and a
    // full SHA-256 every boot is wasted work. Reinstalling the APK
    // bumps the asset's bytes, length almost always differs, and we
    // re-extract.
    if let Ok(meta) = fs::metadata(&target)
        && meta.len() as usize == expected_len
    {
        log::debug!(
            "zd_exec_install: {} up to date ({} bytes); skipping",
            target.display(),
            expected_len,
        );
        return Ok(());
    }

    let mut buf = Vec::with_capacity(expected_len);
    asset.read_to_end(&mut buf)?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    // Write atomically: write to <target>.new, rename into place. The
    // intermediate name avoids leaving a half-written zd-exec if we
    // crash mid-write.
    let staging = target.with_extension("new");
    fs::write(&staging, &buf)
        .with_context(|| format!("write staging {}", staging.display()))?;
    let mut perms = fs::metadata(&staging)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&staging, perms)
        .with_context(|| format!("chmod 0755 {}", staging.display()))?;
    fs::rename(&staging, &target)
        .with_context(|| format!("rename {} -> {}", staging.display(), target.display()))?;

    log::info!(
        "zd_exec_install: extracted {} ({} bytes) -> {}",
        ASSET_NAME,
        expected_len,
        target.display(),
    );
    Ok(())
}

/// Populate `$PREFIX/zd-runtime/` with one symlink per name in
/// `binaries`, each pointing at `../bin/zd-exec`. This is the
/// virtual `/usr/bin` the Zed app process gets to see — kernel PATH
/// lookup of e.g. `java` finds the symlink, exec's `zd-exec`, which
/// dispatches through the active `RuntimeProvider` to wherever the
/// binary actually lives.
///
/// `binaries` should come from `provider.list_binaries()` so the
/// dir mirrors the active adapter's filesystem. No hardcoded list:
/// when the user `apt install`s a new tool inside chroot, the next
/// boot picks it up; when they switch adapters, the symlink set is
/// rewritten to match the new env.
///
/// Idempotent and self-healing:
///
///   - Symlinks pointing at `../bin/zd-exec` whose name is NOT in
///     `binaries` are removed (so adapter switches and uninstalls
///     don't leave dead entries).
///   - Symlinks pointing at `../bin/zd-exec` whose name IS in
///     `binaries` are left alone.
///   - Non-symlink entries (file, dir) are left alone — we don't
///     touch what we didn't create.
///   - Names in `binaries` that don't yet exist are created.
pub fn ensure_runtime_symlinks(prefix: &Path, binaries: &[String]) -> Result<()> {
    let runtime_dir = prefix.join("zd-runtime");
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create_dir_all {}", runtime_dir.display()))?;
    let target = Path::new("../bin/zd-exec");

    let wanted: std::collections::HashSet<&str> =
        binaries.iter().map(String::as_str).collect();

    // Sweep stale: remove zd-exec-shaped symlinks whose name isn't
    // wanted anymore (typical after an adapter switch).
    let mut removed = 0usize;
    if let Ok(entries) = fs::read_dir(&runtime_dir) {
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            if wanted.contains(name.as_str()) {
                continue;
            }
            let path = entry.path();
            // Only remove if it's a symlink pointing where we'd point.
            // Anything else (real file, foreign symlink) is left alone.
            if let Ok(link_target) = fs::read_link(&path)
                && link_target == target
            {
                if let Err(e) = fs::remove_file(&path) {
                    log::warn!(
                        "zd_exec_install: failed to remove stale symlink {}: {e}",
                        path.display(),
                    );
                } else {
                    removed += 1;
                }
            }
        }
    }

    // Create or repair entries for everything in `binaries`.
    let mut created = 0usize;
    let mut already = 0usize;
    let mut skipped = 0usize;
    for name in binaries {
        let link = runtime_dir.join(name);
        match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if fs::read_link(&link).ok().as_deref() == Some(target) {
                    already += 1;
                    continue;
                }
                // Symlink exists but points somewhere unexpected. Replace.
                fs::remove_file(&link).with_context(|| {
                    format!("removing wrong-target symlink {}", link.display())
                })?;
            }
            Ok(_) => {
                // Real file / directory at this name. Don't overwrite.
                skipped += 1;
                continue;
            }
            Err(_) => {} // Doesn't exist yet, create.
        }

        std::os::unix::fs::symlink(target, &link).with_context(|| {
            format!("symlink {} -> {}", link.display(), target.display())
        })?;
        created += 1;
    }

    log::info!(
        "zd_exec_install: zd-runtime ready at {} \
         (requested {}, created {}, already linked {}, removed stale {}, skipped non-symlinks {})",
        runtime_dir.display(),
        binaries.len(),
        created,
        already,
        removed,
        skipped,
    );
    Ok(())
}
