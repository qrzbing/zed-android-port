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
use serde::Deserialize;

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

fn release_api_url(release_repo: &str) -> String {
    format!("https://api.github.com/repos/{release_repo}/releases/latest")
}

/// Minimal subset of the GitHub releases JSON we care about.
#[derive(Debug, Deserialize)]
struct ReleaseManifest {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
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
    progress.step("Resolving latest bootstrap release");
    let manifest = fetch_release_manifest(release_repo)
        .with_context(|| format!("fetch release manifest from {release_repo}"))?;

    let version_file = prefix.join(VERSION_FILE);
    if let Ok(existing) = fs::read_to_string(&version_file)
        && existing.trim() == manifest.tag_name
    {
        log::info!(
            "bootstrap_install: $PREFIX already at {}, skipping install",
            manifest.tag_name
        );
        return Ok(());
    }

    let asset = manifest
        .assets
        .iter()
        .find(|a| a.name == RELEASE_ASSET_NAME)
        .ok_or_else(|| {
            anyhow!(
                "release {} has no asset named {RELEASE_ASSET_NAME}",
                manifest.tag_name
            )
        })?;

    progress.step(&format!(
        "Downloading {RELEASE_ASSET_NAME} ({})",
        manifest.tag_name
    ));
    let zip_bytes = download_asset(&asset.browser_download_url)
        .with_context(|| format!("download {}", asset.browser_download_url))?;
    log::info!(
        "bootstrap_install: downloaded {} bytes from {}",
        zip_bytes.len(),
        asset.browser_download_url,
    );

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
        .write_all(manifest.tag_name.as_bytes())?;

    log::info!(
        "bootstrap_install: bootstrap {} ready at {}",
        manifest.tag_name,
        prefix.display()
    );
    progress.step(&format!("Bootstrap {} installed", manifest.tag_name));
    Ok(())
}

fn fetch_release_manifest(release_repo: &str) -> Result<ReleaseManifest> {
    let url = release_api_url(release_repo);
    let body: String = ureq::get(&url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow!("HTTP GET {url}: {e}"))?
        .into_string()
        .map_err(|e| anyhow!("read body from {url}: {e}"))?;
    let manifest: ReleaseManifest = serde_json::from_str(&body)
        .with_context(|| format!("parse release JSON from {url}"))?;
    Ok(manifest)
}

fn download_asset(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .call()
        .map_err(|e| anyhow!("HTTP GET {url}: {e}"))?;
    let cap = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
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
