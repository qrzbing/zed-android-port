//! Download + extract path for [`super::bootstrap::BootstrapAdapter::install`].
//!
//! Pulls the latest release tarball from `<release_repo>`'s GitHub
//! releases endpoint, downloads `bootstrap-aarch64.zip`, extracts the
//! contents into a staging dir, swaps the staging into `$PREFIX`
//! atomically, and writes a version sentinel so subsequent boots skip
//! re-extraction.
//!
//! Lives in `zdroid_runtime` rather than `gpui_android` so the adapter
//! is self-contained — the historical extraction code in
//! `gpui_android::termux_bootstrap` couples to APK assets and the older
//! bundled-zip flow. Phase 8 of the Termux-divestment refactor sweeps
//! the legacy module; this is the replacement for the install path.

use std::fs;
use std::io::{Cursor, Read, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::health::ProgressSink;

/// Asset name inside the GitHub release. The bootstrap-build pipeline
/// names every release's archive identically; the version is encoded
/// in the release TAG, not the asset filename.
const RELEASE_ASSET_NAME: &str = "bootstrap-aarch64.zip";

/// Manifest entry inside the bootstrap zip carrying symlink targets
/// the zip format itself can't represent on extraction (Android's
/// extract path doesn't preserve mode bits + symlinks the way `unzip`
/// does on a real Unix). Replayed after the regular extract pass.
const SYMLINKS_ENTRY: &str = "SYMLINKS.txt";
const SYMLINKS_DELIM: &str = "←";

/// File at `$PREFIX/.bootstrap-version` recording which release tag is
/// currently extracted. Re-extracts only fire when this doesn't match
/// the latest release tag at download time.
const VERSION_FILE: &str = ".bootstrap-version";

/// Static download URL for the latest release. GitHub 302-redirects
/// this to `/releases/download/<tag>/<filename>` which then 302s to
/// the asset blob on S3. Bypasses `api.github.com` entirely, so the
/// 60-req/hour unauthenticated rate limit doesn't apply.
fn release_download_url(release_repo: &str) -> String {
    format!(
        "https://github.com/{release_repo}/releases/latest/download/{RELEASE_ASSET_NAME}"
    )
}

/// Download the latest release zip + extract into `<prefix>` atomically.
/// Idempotent against `<prefix>/.bootstrap-version` — if the on-disk
/// sentinel matches the latest tag, the function returns Ok without
/// touching the network or filesystem.
pub fn install_latest(
    prefix: &Path,
    release_repo: &str,
    progress: &mut dyn ProgressSink,
) -> Result<()> {
    progress.step("Downloading latest bootstrap");
    let url = release_download_url(release_repo);
    let (zip_bytes, tag_name) =
        download_latest_release(&url).with_context(|| format!("download from {url}"))?;
    log::info!(
        "bootstrap_install: downloaded {} bytes, resolved tag {}",
        zip_bytes.len(),
        tag_name,
    );

    let version_file = prefix.join(VERSION_FILE);
    if let Ok(existing) = fs::read_to_string(&version_file)
        && existing.trim() == tag_name
    {
        log::info!(
            "bootstrap_install: $PREFIX already at {tag_name}, skipping extract"
        );
        progress.step(&format!("Bootstrap {tag_name} already installed"));
        return Ok(());
    }

    progress.step("Extracting bootstrap");
    let staging = prefix.with_extension("staging");
    extract_into_staging(&zip_bytes, &staging)
        .with_context(|| format!("extract into {}", staging.display()))?;

    swap_staging_into_prefix(&staging, prefix)
        .with_context(|| format!("swap staging into {}", prefix.display()))?;

    if let Some(parent) = version_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(&version_file)
        .with_context(|| format!("create version sentinel at {}", version_file.display()))?
        .write_all(tag_name.as_bytes())?;

    log::info!(
        "bootstrap_install: bootstrap {} ready at {}",
        tag_name,
        prefix.display()
    );
    progress.step(&format!("Bootstrap {tag_name} installed"));
    Ok(())
}

/// Resolve the latest-release tag + download the asset.
///
/// Two requests, neither against `api.github.com`:
///   1. `GET /<repo>/releases/latest/download/<file>` with redirects
///      disabled. GitHub returns a 302 whose `Location` is
///      `/<repo>/releases/download/<tag>/<file>` — parse the tag out.
///   2. `GET` the Location URL, following redirects this time, to
///      land on the S3 blob and stream the zip bytes back.
fn download_latest_release(latest_url: &str) -> Result<(Vec<u8>, String)> {
    let agent_no_redirect = ureq::builder().redirects(0).build();
    let head = agent_no_redirect
        .get(latest_url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .call();
    // ureq returns Err(Status(302, _)) here because we asked it not to
    // follow. Pull the Response out of either arm: the Location header
    // is what we need, status doesn't matter.
    let head_resp = match head {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => return Err(anyhow!("HTTP GET {latest_url}: {e}")),
    };
    let tag_url = head_resp
        .header("Location")
        .ok_or_else(|| {
            anyhow!(
                "no Location header on {latest_url}; got status {}",
                head_resp.status()
            )
        })?
        .to_owned();
    let tag = parse_tag_from_download_url(&tag_url).with_context(|| {
        format!(
            "extract release tag from {tag_url}; expected \
             `/releases/download/<tag>/{RELEASE_ASSET_NAME}` segment"
        )
    })?;

    // Step 2: download the actual asset. ureq follows redirects by
    // default, so this lands on the S3 blob.
    let resp = ureq::get(&tag_url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .call()
        .map_err(|e| anyhow!("HTTP GET {tag_url}: {e}"))?;
    let cap = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    resp.into_reader().read_to_end(&mut buf)?;
    Ok((buf, tag))
}

/// Pull the release tag out of a per-tag download URL like
/// `https://.../releases/download/<tag>/<filename>`. The tag segment
/// sits between literal `/releases/download/` and `/<filename>`.
fn parse_tag_from_download_url(url: &str) -> Result<String> {
    let marker = "/releases/download/";
    let after = url
        .find(marker)
        .map(|i| &url[i + marker.len()..])
        .ok_or_else(|| anyhow!("missing /releases/download/ in {url}"))?;
    let tag = after
        .split('/')
        .next()
        .ok_or_else(|| anyhow!("empty tag segment in {url}"))?;
    if tag.is_empty() {
        return Err(anyhow!("empty tag segment in {url}"));
    }
    Ok(tag.to_owned())
}

fn extract_into_staging(zip_bytes: &[u8], staging: &Path) -> Result<()> {
    if staging.exists() {
        fs::remove_dir_all(staging)
            .with_context(|| format!("wipe leftover staging at {}", staging.display()))?;
    }
    fs::create_dir_all(staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;

    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .context("ZipArchive::new on downloaded bootstrap")?;

    let symlinks = extract_entries(&mut archive, staging)?;
    log::info!(
        "bootstrap_install: extracted {} entries, {} symlinks queued",
        archive.len(),
        symlinks.len(),
    );
    replay_symlinks(staging, &symlinks)?;
    Ok(())
}

fn swap_staging_into_prefix(staging: &Path, prefix: &Path) -> Result<()> {
    if prefix.exists() {
        fs::remove_dir_all(prefix)
            .with_context(|| format!("wipe old prefix at {}", prefix.display()))?;
    }
    fs::rename(staging, prefix).with_context(|| {
        format!("rename {} -> {}", staging.display(), prefix.display())
    })?;
    Ok(())
}

fn extract_entries<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    staging: &Path,
) -> Result<Vec<(String, String)>> {
    let mut symlinks = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_owned();

        if raw_name == SYMLINKS_ENTRY {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            for line in text.lines() {
                if line.is_empty() {
                    continue;
                }
                let Some((target, link_rel)) = line.split_once(SYMLINKS_DELIM) else {
                    log::warn!("bootstrap_install: malformed SYMLINKS.txt line: {line:?}");
                    continue;
                };
                symlinks.push((target.to_owned(), link_rel.to_owned()));
            }
            continue;
        }

        let Some(safe) = entry.enclosed_name() else {
            log::warn!("bootstrap_install: skipping unsafe entry path {raw_name:?}");
            continue;
        };
        let dest: PathBuf = staging.join(&safe);

        if entry.is_dir() {
            fs::create_dir_all(&dest)?;
            continue;
        }

        if entry.is_symlink() {
            log::warn!(
                "bootstrap_install: unexpected inline symlink entry {raw_name:?}; \
                 skipping (symlinks come via SYMLINKS.txt)"
            );
            continue;
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let entry_mode = entry.unix_mode();
        let mut out =
            fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;

        if let Some(mode) = entry_mode {
            let owner_only =
                (mode & 0o700) | if mode & 0o100 != 0 { 0o700 } else { 0o600 };
            let mut perms = fs::metadata(&dest)?.permissions();
            perms.set_mode(owner_only);
            fs::set_permissions(&dest, perms)?;
        } else if raw_name.starts_with("bin/")
            || raw_name.starts_with("libexec/")
            || raw_name.starts_with("lib/apt/methods/")
            || raw_name == "lib/apt/apt-helper"
        {
            let mut perms = fs::metadata(&dest)?.permissions();
            perms.set_mode(0o700);
            fs::set_permissions(&dest, perms)?;
        }
    }

    Ok(symlinks)
}

fn replay_symlinks(staging: &Path, symlinks: &[(String, String)]) -> Result<()> {
    for (target, link_rel) in symlinks {
        let link_rel = link_rel.trim_start_matches("./");
        let link_abs = staging.join(link_rel);
        if let Some(parent) = link_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        if link_abs.exists() || link_abs.symlink_metadata().is_ok() {
            fs::remove_file(&link_abs).ok();
        }
        std::os::unix::fs::symlink(target, &link_abs)
            .with_context(|| format!("symlink {} -> {}", link_abs.display(), target))?;
    }
    Ok(())
}

/// `ProgressSink` impl that routes progress events to logcat. Useful
/// when the caller is happy with log-only feedback (e.g. headless
/// first-boot install before any UI is up to render a progress bar).
#[derive(Debug)]
pub struct LogProgressSink;

impl ProgressSink for LogProgressSink {
    fn step(&mut self, label: &str) {
        log::info!("bootstrap_install: {label}");
    }
    fn progress(&mut self, done: u64, total: u64) {
        if total > 0 {
            log::debug!("bootstrap_install: {done}/{total}");
        }
    }
    fn warn(&mut self, message: &str) {
        log::warn!("bootstrap_install: {message}");
    }
}
