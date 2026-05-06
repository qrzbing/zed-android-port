//! First-launch extractor for the bundled Termux bootstrap.
//!
//! 1:1 port of Termux's `TermuxInstaller.java`. The bundled bootstrap zip
//! lives in `assets/bootstrap-aarch64.zip`, manually SCP'd from the Vultr
//! build host after each rebuild — there is no Gradle download task yet.
//! Every binary inside the bootstrap has
//! `/data/data/com.zdroid/files/usr/...` baked into its
//! `DT_RUNPATH` and shebangs (the `build-bootstraps.sh:246` sed pass
//! rewrites the upstream `com.termux` strings before zipping).
//!
//! ## Where this writes
//!
//! `$PREFIX = <data_path>/usr` is the *only* directory this module ever
//! mutates. `$HOME = <data_path>/home` is never touched here — that lets us
//! re-extract on version mismatch without nuking the user's git repos,
//! shell history, or dotfiles. (User-installed `pkg install ...` packages
//! under `$PREFIX/...` *will* be wiped on re-extract, matching upstream
//! Termux behaviour. The contract is "$PREFIX is owned by the bootstrap;
//! $HOME is owned by the user".)
//!
//! ## Atomicity
//!
//! Sequence is **not** atomic: `wipe(staging) → extract → wipe(prefix) →
//! rename(staging, prefix) → write sentinel`. There is a window after the
//! prefix wipe where `$PREFIX` is gone. A power loss / SIGKILL inside that
//! window leaves a half-installed runtime, but the version sentinel is
//! written *last*, so the next boot's `read(version_file)` fails and we
//! re-extract from scratch. Recoverable, not atomic. Comments don't
//! oversell.

use std::ffi::CString;
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use android_activity::AndroidApp;
use anyhow::{Context, Result, anyhow};

const ASSET_NAME: &str = "bootstrap-aarch64.zip";
const SYMLINKS_ENTRY: &str = "SYMLINKS.txt";

// SYMLINKS.txt delimiter is U+2190 LEFTWARDS ARROW (UTF-8: 0xE2 0x86 0x90).
// Verified against `bootstrap-2026.04.26-r1+apt.android-7/SYMLINKS.txt` —
// `xxd` shows `e2 86 90` between the absolute-target and relative-link
// halves of every line. The delimiter is *not* a tab and not a NUL.
const SYMLINKS_DELIM: char = '\u{2190}';

const VERSION_FILE: &str = "etc/termux-zed-bootstrap.version";

// Bumped whenever we reroll the rebuilt bootstrap. On mismatch the
// extractor wipes $PREFIX and re-runs. This is the source of truth for
// what we expect; the actual zip at
// `app/src/main/assets/bootstrap-aarch64.zip` is whatever was last SCP'd
// from Vultr — Rust ↔ asset skew is possible if the SCP is skipped.
//
// r5 (2026-05-03): r4 + `grep -lI` flag in build-bootstraps.sh path-fixup
// so sed skips ELF binaries. r4's path-fixup corrupted proot's ELF
// section table by replacing `com.termux` strings inside its data
// section (9-byte length delta shifted offsets). r5 keeps shell
// scripts/configs path-corrected while leaving binaries untouched.
// r6 (2026-05-03): drops proot entirely. dpkg patched at
// `lib/dpkg/tarfn.c` to rewrite `data/data/com.termux/...` paths to our
// app's data dir at extract time, reading `TERMUX_APP__PACKAGE_NAME`
// from env. Upstream Termux .debs (`pkg install rust-analyzer` etc.)
// install natively without ptrace overhead. See
// `crates/gpui_android/termux-patches/dpkg/lib-dpkg-tarfn.c.patch`.
// r7 (2026-05-03): same bootstrap zip as r6 (no Vultr rebuild). Bump
// triggered by an apt-install-dpkg disaster: empirical user session
// proved that `apt install dpkg` (or apt --fix-broken install when
// dpkg is in the dependency closure) replaces our patched dpkg with
// upstream's, which has com.termux baked into RUNPATH/sysconfdir/...
// and immediately bricks. Recovery is re-extract. r7 forces it once;
// `apply_runtime_patches` now also writes
// `etc/apt/preferences.d/zed-pin-dpkg` to prevent re-clobbering.
// r8 (2026-05-03): fresh bootstrap built on Vultr with `--add patchelf`
// so we have the binary needed for the layer-3 fix. `apply_runtime_
// patches` now installs `etc/apt/zed-patchelf-hook.sh` plus an
// `etc/apt/apt.conf.d/98-zed-patchelf` DPkg::Post-Invoke hook that
// rewrites DT_RUNPATH on every freshly-installed upstream binary so
// `pkg install nodejs` etc. produce binaries that can actually run.
// r9 (2026-05-03): same bootstrap as r8 (no rebuild). Bumps version to
// force re-extract because r8's patchelf hook briefly wrote
// /data/user/0/<pkg>/files/usr/lib into bootstrap libs' RPATH (the
// Android-resolved form), creating a dynamic-linker namespace
// mismatch with bash's /data/data/<pkg>/... RUNPATH that bricked
// bash startup. Helper script now canonicalizes to /data/data form.
// r10 (2026-05-03): r9's helper still corrupted libandroid-support.so:
// patchelf --force-rpath truncated the file from 66KB to 21KB during
// the first apt --fix-broken install (file got caught by -mmin -10
// because bootstrap was extracted ~5 min earlier). Three fixes:
// (a) drop --force-rpath, (b) skip files whose RUNPATH already matches
// our prefix (so bootstrap libs are never touched), (c) tighten
// -mmin -10 → -mmin -1 (only catch dpkg's just-extracted files).
// r11 (2026-05-03): r10 + the layer-4/5 systematization. The bootstrap
// now ships `ld-musl-aarch64.so.1` extracted from Alpine's musl APK
// (~700KB) so musl-linked upstream binaries (claude-code, alpine-built
// Rust/Bun tools) run natively after patchelf. apply_runtime_patches
// also writes `$PREFIX/bin/zed-setup-claude` — a one-shot helper that
// turns `npm install -g @anthropic-ai/claude-code` into a runnable
// `claude` command (musl variant, install.cjs map, patchelf, wrapper).
const BOOTSTRAP_VERSION: &str = "2026.04.26-r1+apt.android-7-zed-r12+com.zdroid";

static EXTRACTED: OnceLock<()> = OnceLock::new();

/// Extract the bundled bootstrap zip into `$PREFIX = data_path/usr` if it
/// isn't already at the bundled version. Idempotent across activity
/// recreation: the OnceLock + on-disk version sentinel both short-circuit
/// re-runs.
///
/// Failure surfaces via `Err` but the caller should treat it as
/// non-fatal — the editor (L1) keeps working without a runtime; only the
/// integrated terminal and `pkg install`-driven LSPs become unavailable.
pub fn extract_if_needed(android_app: &AndroidApp, data_path: &Path) -> Result<()> {
    if EXTRACTED.get().is_some() {
        return Ok(());
    }

    let prefix = data_path.join("usr");
    let staging = data_path.join("usr-staging");
    let version_file = prefix.join(VERSION_FILE);

    if let Ok(existing) = std::fs::read_to_string(&version_file) {
        if existing.trim() == BOOTSTRAP_VERSION {
            log::info!(
                "termux_bootstrap: $PREFIX already at {BOOTSTRAP_VERSION}, skipping extract"
            );
            let _ = EXTRACTED.set(());
            return Ok(());
        }
        log::warn!(
            "termux_bootstrap: version mismatch (have {:?}, want {:?}); re-extracting. \
             User-installed packages under $PREFIX will be wiped; $HOME is preserved.",
            existing.trim(),
            BOOTSTRAP_VERSION,
        );
    }

    // Open the asset *first*: if the bootstrap isn't bundled (pre-L2a) we
    // bail out without creating a `usr-staging` directory, so a fresh data
    // dir doesn't get a phantom empty folder that confuses anyone listing
    // $ROOTFS later.
    log::info!("termux_bootstrap: opening {ASSET_NAME} from APK assets");
    let bytes = read_bootstrap_asset(android_app)?;
    log::info!("termux_bootstrap: read {} bytes from asset, parsing zip", bytes.len());

    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("wipe leftover staging at {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .context("ZipArchive::new on bootstrap asset")?;

    let symlinks = extract_entries(&mut archive, &staging)?;
    log::info!(
        "termux_bootstrap: extracted {} entries, {} symlinks queued",
        archive.len(),
        symlinks.len(),
    );

    replay_symlinks(&staging, &symlinks)?;

    if prefix.exists() {
        std::fs::remove_dir_all(&prefix)
            .with_context(|| format!("wipe old prefix at {}", prefix.display()))?;
    }
    std::fs::rename(&staging, &prefix).with_context(|| {
        format!(
            "rename {} -> {}",
            staging.display(),
            prefix.display()
        )
    })?;

    // Drop the musl dynamic linker into our prefix. Termux's bionic libc
    // and glibc are ABI-incompatible, but musl is small and self-
    // contained (linker IS libc — one file does both jobs). Shipping
    // ld-musl-aarch64.so.1 lets `pkg install`-installed musl binaries
    // (e.g. claude-code-linux-arm64-musl) run natively after the
    // patchelf hook rewrites their `--set-interpreter` to this path.
    if let Err(err) = install_musl_linker(android_app, &prefix) {
        log::warn!(
            "termux_bootstrap: musl linker install failed: {err:#}; \
             pkg install of musl-linked upstream binaries will need \
             a manual ld-musl-aarch64.so.1 in $PREFIX/lib"
        );
    }

    if let Some(parent) = version_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(&version_file)
        .with_context(|| format!("create version sentinel at {}", version_file.display()))?
        .write_all(BOOTSTRAP_VERSION.as_bytes())?;

    log::info!(
        "termux_bootstrap: bootstrap {BOOTSTRAP_VERSION} ready at {}",
        prefix.display()
    );
    let _ = EXTRACTED.set(());
    Ok(())
}

fn read_bootstrap_asset(android_app: &AndroidApp) -> Result<Vec<u8>> {
    let asset_manager = android_app.asset_manager();
    let asset_name = CString::new(ASSET_NAME)?;
    let mut asset = asset_manager
        .open(&asset_name)
        .ok_or_else(|| anyhow!("bootstrap asset {ASSET_NAME} not present in APK"))?;
    let mut buf = Vec::with_capacity(asset.length());
    asset.read_to_end(&mut buf)?;
    Ok(buf)
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
                    log::warn!("termux_bootstrap: malformed SYMLINKS.txt line: {line:?}");
                    continue;
                };
                symlinks.push((target.to_owned(), link_rel.to_owned()));
            }
            continue;
        }

        // Defense-in-depth against zip-slip even though we're shipping our
        // own bootstrap. `enclosed_name` strips `..` components.
        let Some(safe) = entry.enclosed_name() else {
            log::warn!("termux_bootstrap: skipping unsafe entry path {raw_name:?}");
            continue;
        };
        let dest: PathBuf = staging.join(&safe);

        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }

        if entry.is_symlink() {
            // Bootstrap zips put symlinks in SYMLINKS.txt, not as zip
            // entries. If we ever see one inline, log so we know our
            // assumption broke.
            log::warn!("termux_bootstrap: unexpected inline symlink entry {raw_name:?}; skipping");
            continue;
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut out = std::fs::File::create(&dest)
            .with_context(|| format!("create {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;

        // Mirror TermuxInstaller.java::setupExecutables. Native binaries
        // and apt helpers need owner-execute. 0o700 keeps everything in
        // the app sandbox; broader perms are pointless because Android's
        // app-private dir is already root:app_<uid>:S0:c... isolated.
        if raw_name.starts_with("bin/")
            || raw_name.starts_with("libexec/")
            || raw_name.starts_with("lib/apt/methods/")
            || raw_name == "lib/apt/apt-helper"
        {
            let mut perms = std::fs::metadata(&dest)?.permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(&dest, perms)?;
        }
    }

    Ok(symlinks)
}

/// Closes the maintainer-script content gap left by our dpkg path-rewrite
/// patches. Those patches handle file PATHS at extract time (so files land
/// at /data/data/com.zdroid/... instead of /data/data/com.termux/...),
/// but the SHEBANG line and inline path strings inside preinst/postinst/
/// prerm/postrm scripts still hardcode "/data/data/com.termux/files/...".
/// dpkg fails to execve them with EACCES because the kernel can't find
/// the (nonexistent for our UID) /data/data/com.termux/files/usr/bin/bash
/// path in the shebang.
///
/// Two parts:
///   1. Rewrites the bootstrap's pre-installed maintainer scripts in
///      $PREFIX/var/lib/dpkg/info/. The bootstrap-build sed pass at
///      build-bootstraps.sh:246 misses these three files (libcompiler-rt's
///      postinst+prerm and termux-tools's preinst — empirical, not by
///      design); rewriting them client-side is the catch-all.
///   2. Installs $PREFIX/etc/apt/apt.conf.d/99-zed-rewrite-postinst, an
///      apt DPkg::Post-Invoke hook that re-runs the same sed over
///      maintainer scripts after every `apt unpack`. That bridges the
///      gap between dpkg --unpack (paths get rewritten by our patch but
///      content doesn't) and dpkg --configure (which would otherwise hit
///      the same EACCES on freshly-installed upstream debs).
///
/// Idempotent and safe to run on every boot — the sed is no-op on
/// already-clean files, and the apt config write is a constant string.
pub fn apply_runtime_patches(data_path: &Path) -> Result<()> {
    let prefix = data_path.join("usr");
    rewrite_maintainer_scripts(&prefix.join("var/lib/dpkg/info"))?;
    install_apt_rewrite_hook(&prefix)?;
    install_apt_dpkg_pin(&prefix)?;
    install_apt_patchelf_hook(&prefix)?;
    install_apt_pre_install_hook(&prefix)?;
    install_apt_node_platform_hook(&prefix)?;
    install_profile_d_init(data_path, &prefix)?;
    patch_node_platform_now(&prefix);
    install_npm_launcher_generator(&prefix)?;
    install_npm_wrapper(&prefix)?;
    cleanup_legacy_claude_wrapper(&prefix);
    run_launcher_generator(&prefix);
    Ok(())
}

/// Make `bash -l` self-bootstrap PREFIX/PATH/HOME/TMPDIR even when launched
/// from a context that didn't inherit our `android_main` env (e.g. `adb shell
/// run-as com.zdroid /path/to/bash -l`, or any subprocess spawned via a path
/// that strips env). Termux's stock `/etc/profile` only sources `profile.d/*.sh`
/// — neither it nor any default profile.d entry sets PREFIX/PATH; Termux's
/// own Android app does that in its Java launcher *before* exec'ing bash.
/// Our gpui app does the same for the gpui process tree but a sideband bash
/// (debugger, adb, integrated-terminal recovery path) doesn't go through
/// `android_main`, so it gets Android's default `/system/bin:/vendor/bin:...`
/// PATH and `apt`/`pkg`/etc. resolve to "command not found".
///
/// The shim guards every export with `[ -z "$VAR" ]` so a properly-set parent
/// env is never overridden — purely additive when something's missing.
/// Idempotent: re-running `apply_runtime_patches` overwrites with identical
/// content, mtime updates harmlessly.
fn install_profile_d_init(data_path: &Path, prefix: &Path) -> Result<()> {
    let profile_d = prefix.join("etc/profile.d");
    if !profile_d.is_dir() {
        return Ok(());
    }
    let target = profile_d.join("zed-init.sh");
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());
    let rootfs_str = data_path.to_string_lossy();
    let rootfs_str = rootfs_str
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| rootfs_str.to_string());
    // Match Android's resolved /data/user/0/<pkg> form too — some launchers
    // hand bash that variant of HOME and we want to override it to our
    // Termux-style $TERMUX__ROOTFS/home regardless of which form Android
    // chose to deliver.
    let android_home_user_form = format!("/data/user/0/{}", data_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default());
    let body = format!(
        "# Auto-generated by gpui_android termux_bootstrap. Self-bootstrap\n\
         # PREFIX/PATH/HOME for any bash -l whose parent didn't already set\n\
         # them (adb run-as, sideband subprocess spawns, etc.).\n\
         if [ -z \"$PREFIX\" ]; then\n    \
             export PREFIX=\"{prefix_str}\"\n\
         fi\n\
         if [ -z \"$TERMUX__PREFIX\" ]; then\n    \
             export TERMUX__PREFIX=\"$PREFIX\"\n\
         fi\n\
         if [ -z \"$TERMUX__ROOTFS\" ]; then\n    \
             export TERMUX__ROOTFS=\"{rootfs_str}\"\n\
         fi\n\
         if [ -z \"$HOME\" ] || [ \"$HOME\" = \"{android_home_user_form}\" ] || [ \"$HOME\" = \"{rootfs_str}\" ]; then\n    \
             export HOME=\"$TERMUX__ROOTFS/home\"\n\
         fi\n\
         if [ -z \"$TMPDIR\" ]; then\n    \
             export TMPDIR=\"$PREFIX/tmp\"\n\
         fi\n\
         if [ -z \"$LANG\" ]; then\n    \
             export LANG=\"en_US.UTF-8\"\n\
         fi\n\
         case \":$PATH:\" in\n    \
             *\":$PREFIX/bin:\"*) ;;\n    \
             *) export PATH=\"$PREFIX/bin:$PATH\" ;;\n\
         esac\n",
    );
    let needs_write = match std::fs::read(&target) {
        Ok(existing) => existing != body.as_bytes(),
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&target, body.as_bytes())
            .with_context(|| format!("write {}", target.display()))?;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&target, perms)?;
        log::info!(
            "termux_bootstrap: wrote profile.d init shim at {}",
            target.display()
        );
    }
    Ok(())
}

/// Fire the launcher generator helper script once at boot. Idempotent —
/// no-ops if every npm-installed binary is already correctly wrapped.
/// We invoke it post-cleanup so any chain we just unwound (binary
/// restored to its original name) gets a single clean `<bin> -> wrapper
/// + <bin>.real -> binary` wrap immediately, instead of waiting for the
/// next npm or apt op to fire the hook.
fn run_launcher_generator(prefix: &Path) {
    let helper = prefix.join("etc/apt/zed-launcher-gen.sh");
    if !helper.is_file() {
        return;
    }
    match std::process::Command::new(&helper)
        .env("PREFIX", prefix)
        .output()
    {
        Ok(out) if out.status.success() => log::info!(
            "termux_bootstrap: launcher generator ran at boot (exit 0)"
        ),
        Ok(out) => log::warn!(
            "termux_bootstrap: launcher generator exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(err) => log::warn!(
            "termux_bootstrap: spawn launcher generator: {err:#}"
        ),
    }
}

/// Remove leftover state from the now-obsolete zed-setup-claude path.
///
/// Until L4 landed, claude was set up by a per-tool script that wrote a
/// proot wrapper at $PREFIX/bin/claude pointing at the JS dispatcher
/// (claude-code/bin/claude.exe). The deep-walk launcher generator now
/// wraps claude correctly at the real binary's path inside the
/// optional-dep package directory. Both wrappers active = double proot
/// (one wrapping node + JS dispatch, one wrapping the real binary)
/// which made `claude` take 1+ minute to start.
///
/// Idempotent: removes the legacy wrapper script if present so the next
/// `npm install -g @anthropic-ai/claude-code` (or post-install hook
/// firing on the existing tree) lets npm restore its own bin symlink,
/// which the launcher generator then leaves alone (claude.exe is JS, not
/// ELF — generator's classify_and_wrap returns early). Also removes the
/// zed-setup-claude script itself so users who did `pkg upgrade` of
/// nodejs/claude-code on stale state don't re-trigger the old path.
fn cleanup_legacy_claude_wrapper(prefix: &Path) {
    let setup_script = prefix.join("bin/zed-setup-claude");
    if setup_script.is_file() {
        if let Err(err) = std::fs::remove_file(&setup_script) {
            log::warn!(
                "termux_bootstrap: remove zed-setup-claude script: {err:#}"
            );
        } else {
            log::info!(
                "termux_bootstrap: removed obsolete zed-setup-claude script at {}",
                setup_script.display()
            );
        }
    }

    // Unwind any stacked .real-chains the launcher generator produced
    // before its `find` filter excluded `*.real`. Each run of the deep
    // walk used to find the previously-renamed binary, wrap it again,
    // and shift the chain one level deeper:
    //   claude              -> wrapper -> claude.real
    //   claude.real         -> wrapper -> claude.real.real
    //   claude.real.real    -> wrapper -> claude.real.real.real
    //   ... claude.real{N}  -> 241 MB ELF (the actual binary)
    //
    // For every `<base>` that has any `<base>.real*` siblings, find the
    // deepest `.real`-suffixed file (the real ELF), delete every shell
    // wrapper between it and the un-suffixed base, rename the ELF back
    // to `<base>`. The next launcher-gen run sees a clean `<base>` and
    // wraps it to a single level of `<base> -> <base>.real`.
    if let Ok(walker) = std::process::Command::new("find")
        .arg(prefix.join("lib/node_modules"))
        .args(["-name", "*.real", "-type", "f"])
        .output()
    {
        let mut stripped_bases = std::collections::HashSet::new();
        for line in walker.stdout.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let p = match std::str::from_utf8(line) {
                Ok(s) => Path::new(s).to_path_buf(),
                Err(_) => continue,
            };
            let s = p.to_string_lossy();
            let mut base_str = s.as_ref();
            while let Some(stripped) = base_str.strip_suffix(".real") {
                base_str = stripped;
            }
            if !stripped_bases.insert(base_str.to_string()) {
                continue;
            }
            let base = Path::new(base_str).to_path_buf();
            let mut deepest = base.clone();
            let mut probe_str = base_str.to_string();
            loop {
                let next = format!("{probe_str}.real");
                if !Path::new(&next).is_file() {
                    break;
                }
                deepest = Path::new(&next).to_path_buf();
                probe_str = next;
            }
            if deepest == base {
                continue;
            }
            let deepest_str = deepest.to_string_lossy().into_owned();
            let mut wrapper_str = base_str.to_string();
            while wrapper_str != deepest_str {
                if Path::new(&wrapper_str).is_file() {
                    let _ = std::fs::remove_file(Path::new(&wrapper_str));
                }
                wrapper_str = format!("{wrapper_str}.real");
            }
            if let Err(err) = std::fs::rename(&deepest, &base) {
                log::warn!(
                    "termux_bootstrap: restore {} <- {}: {err:#}",
                    base.display(),
                    deepest.display()
                );
            } else {
                log::info!(
                    "termux_bootstrap: restored stacked-wrapper binary at {}",
                    base.display()
                );
            }
        }
    }

    // Always drop $PREFIX/bin/claude regardless of marker — it's npm's
    // territory and any leftover wrapper here is from the obsolete
    // zed-setup-claude path. Next `npm install -g @anthropic-ai/claude-
    // code` (or any npm op now that the wrapper fires the launcher
    // generator) will recreate the symlink, the deep-walk will wrap the
    // optional-dep binary correctly, and the JS dispatch in claude.exe
    // (left alone because it's not ELF) will spawn the deep-walk wrapper.
    let bin_claude = prefix.join("bin/claude");
    if let Ok(meta) = std::fs::symlink_metadata(&bin_claude) {
        if !meta.file_type().is_symlink() {
            if let Err(err) = std::fs::remove_file(&bin_claude) {
                log::warn!(
                    "termux_bootstrap: remove $PREFIX/bin/claude: {err:#}"
                );
            } else {
                log::info!(
                    "termux_bootstrap: removed legacy $PREFIX/bin/claude wrapper"
                );
            }
        }
    }

    // Sweep stale shell wrappers left in node_modules from the old
    // proot-wrap path. The L4g hex-patch flow patches binaries in place
    // and leaves no wrappers in node_modules, so any `Auto-generated`
    // wrapper file there points at a path that no longer exists (or at
    // best is redundant). Delete them; npm symlinks pointing at
    // now-deleted wrappers will dangle until the next `npm install -g`,
    // which is fine — the user already has to reinstall to pick up the
    // hex-patch anyway, and the dangling symlink is a clearer signal
    // than a wrapper that proot-errors at runtime.
    if let Ok(walker) = std::process::Command::new("find")
        .arg(prefix.join("lib/node_modules"))
        .args(["-type", "f", "-size", "-5k"])
        .output()
    {
        for line in walker.stdout.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let path = match std::str::from_utf8(line) {
                Ok(s) => Path::new(s).to_path_buf(),
                Err(_) => continue,
            };
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !content.starts_with("#!") {
                continue;
            }
            if !content.contains("Auto-generated by zed") {
                continue;
            }
            if let Err(err) = std::fs::remove_file(&path) {
                log::warn!(
                    "termux_bootstrap: remove stale wrapper {}: {err:#}",
                    path.display()
                );
            } else {
                log::info!(
                    "termux_bootstrap: removed stale auto-gen wrapper at {}",
                    path.display()
                );
            }
        }
    }
}

/// Replace `$PREFIX/bin/npm` symlink with a shell shim that forwards args
/// to real npm and fires the launcher generator on success.
///
/// Why: every npm-distributed CLI (claude, codex, future tools) lands a
/// binary under `$PREFIX/lib/node_modules/<pkg>/bin/<name>` and a symlink
/// at `$PREFIX/bin/<name>`. Some of those binaries hardcode Linux paths
/// (Bun-compiled tools embedding `/etc/resolv.conf` for DNS) that don't
/// exist on Android. Without intercepting npm, we'd write a per-tool
/// `zed-setup-X` script for each — death by a thousand patches. This shim
/// runs the launcher generator after every successful npm op so the right
/// runtime wrapper is auto-emitted (proot bind for static-with-hardcoded
/// paths; direct exec for everything else).
///
/// Self-healing: re-installed every `apply_runtime_patches` boot in case
/// `pkg install nodejs` or `npm install -g npm` clobbers the symlink. The
/// shim itself is plain forwarding shell so it can't break npm CLI usage
/// — argv is passed verbatim, exit code preserved.
fn install_npm_wrapper(prefix: &Path) -> Result<()> {
    let bin_dir = prefix.join("bin");
    if !bin_dir.is_dir() {
        return Ok(());
    }
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());

    let body = format!(
        "#!{prefix_str}/bin/sh\n\
         # Auto-generated by gpui_android termux_bootstrap. Forwards to real\n\
         # npm and fires the launcher generator on success so newly-installed\n\
         # CLI tools get the right runtime wrapper (proot for static binaries\n\
         # with hardcoded Linux paths, otherwise direct exec via npm's own\n\
         # symlink). Self-healing — re-installed at every boot.\n\
         REAL_NPM_JS={prefix_str}/lib/node_modules/npm/bin/npm-cli.js\n\
         NODE={prefix_str}/bin/node\n\
         HOOK={prefix_str}/etc/apt/zed-launcher-gen.sh\n\
         if [ ! -x \"$NODE\" ] || [ ! -f \"$REAL_NPM_JS\" ]; then\n    \
             echo \"zed-npm: real npm or node missing\" >&2\n    \
             exit 1\n\
         fi\n\
         \"$NODE\" \"$REAL_NPM_JS\" \"$@\"\n\
         RC=$?\n\
         if [ -x \"$HOOK\" ]; then\n    \
             \"$HOOK\" 2>&1 || true\n\
         fi\n\
         exit $RC\n"
    );

    let wrapper_path = bin_dir.join("npm");
    let needs_install = match std::fs::symlink_metadata(&wrapper_path) {
        Ok(meta) if meta.file_type().is_symlink() => true,
        Ok(_) => match std::fs::read(&wrapper_path) {
            Ok(existing) => existing != body.as_bytes(),
            Err(_) => true,
        },
        Err(_) => true,
    };
    if !needs_install {
        return Ok(());
    }
    let _ = std::fs::remove_file(&wrapper_path);
    std::fs::write(&wrapper_path, body.as_bytes())
        .with_context(|| format!("write {}", wrapper_path.display()))?;
    let mut perms = std::fs::metadata(&wrapper_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&wrapper_path, perms)?;
    log::info!(
        "termux_bootstrap: installed npm wrapper at {}",
        wrapper_path.display()
    );
    Ok(())
}

/// Walk `$PREFIX/bin/` symlinks pointing into `$PREFIX/lib/node_modules/`,
/// classify the target binary, and emit the right runtime wrapper.
///
/// Generates a single helper script `$PREFIX/etc/apt/zed-launcher-gen.sh`
/// that the npm wrapper invokes after every npm op. The script:
///
/// 1. Walks `$PREFIX/bin/*` symlinks
/// 2. Filters to those resolving into `$PREFIX/lib/node_modules/`
/// 3. For each: detects ELF interpreter, fixes musl `INTERP` to point at
///    `$PREFIX/lib/ld-musl-aarch64.so.1` (where we actually ship the
///    linker, vs. the literal `/lib/...` the binary asks for)
/// 4. greps the binary for hardcoded `/etc/resolv.conf`
/// 5. Replaces the symlink with a wrapper that does `proot -b
///    $PREFIX/etc/resolv.conf:/etc/resolv.conf <real-bin>` if the binary
///    hardcodes that path; otherwise leaves npm's symlink alone
/// 6. For glibc-dynamic targets, emits a `grun`-prefixed wrapper if
///    `glibc-runner` is installed; otherwise stubs with a clear install
///    instruction so the user knows what to do
///
/// Idempotent: re-running with no node_modules changes is a no-op (each
/// wrapper body comparison short-circuits if already correct).
///
/// Invoked from: `install_npm_wrapper`'s shim post-success path. Also
/// safe to invoke directly from a terminal for debugging.
fn install_npm_launcher_generator(prefix: &Path) -> Result<()> {
    let etc_apt = prefix.join("etc/apt");
    if !etc_apt.is_dir() {
        return Ok(());
    }
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());

    let body = format!(
        "#!{prefix_str}/bin/sh\n\
         # Auto-generated by gpui_android termux_bootstrap. For each ELF in\n\
         # $PREFIX/bin/* symlinks (resolved into node_modules) and any deep\n\
         # ELF under $PREFIX/lib/node_modules/**:\n\
         #   1. patchelf musl interp /lib/ld-musl-aarch64.so.1 ->\n\
         #      $PREFIX/lib/ld-musl-aarch64.so.1 so the kernel can find the\n\
         #      linker (Android has no /lib).\n\
         #   2. hex-patch the rodata literal /etc/resolv.conf to\n\
         #      /sdcard/.zed/r (14 bytes + 2 NUL pad fits the 16-byte slot).\n\
         #      gpui_android::dns_bridge populates /sdcard/.zed/r at boot\n\
         #      with Android's active DNS servers.\n\
         #   3. for glibc-dynamic targets, write a grun-prefixed launcher\n\
         #      at the npm symlink path (or a stub if grun isn't installed).\n\
         # Patched binaries do outbound UDP DNS to the nameserver in\n\
         # /sdcard/.zed/r — no proot wrap, no ptrace overhead, no\n\
         # telemetry-storm amplification. Idempotent: rerunning is free.\n\
         set -u\n\
         PREFIX={prefix_str}\n\
         [ -d \"$PREFIX/bin\" ] || exit 0\n\
         [ -d \"$PREFIX/lib/node_modules\" ] || exit 0\n\
         [ -x \"$PREFIX/bin/readelf\" ] || exit 0\n\
         \n\
         write_if_changed() {{\n    \
             dst=\"$1\"; want=\"$2\"\n    \
             if [ -f \"$dst\" ] && [ \"$(cat -- \"$dst\" 2>/dev/null)\" = \"$want\" ]; then\n        \
                 return 0\n    \
             fi\n    \
             rm -f -- \"$dst\"\n    \
             printf '%s\\n' \"$want\" > \"$dst\"\n    \
             chmod +x -- \"$dst\"\n\
         }}\n\
         \n\
         patch_musl_interp() {{\n    \
             bin=\"$1\"\n    \
             [ -x \"$PREFIX/bin/patchelf\" ] || return 0\n    \
             interp=$(\"$PREFIX/bin/readelf\" -l \"$bin\" 2>/dev/null | awk '/interpreter:/ {{ gsub(/[\\[\\]]/, \"\", $NF); print $NF; exit }}')\n    \
             case \"$interp\" in\n        \
                 /lib/ld-musl-aarch64.so.1)\n            \
                     \"$PREFIX/bin/patchelf\" --set-interpreter \"$PREFIX/lib/ld-musl-aarch64.so.1\" \"$bin\" 2>/dev/null || true\n            \
                     \"$PREFIX/bin/patchelf\" --set-rpath \"$PREFIX/lib\" \"$bin\" 2>/dev/null || true\n            \
                     ;;\n    \
             esac\n\
         }}\n\
         \n\
         # Hex-patch /etc/resolv.conf -> /sdcard/.zed/r in the binary's\n\
         # .rodata. /etc/resolv.conf is 16 bytes; /sdcard/.zed/r is 14 +\n\
         # 2 NUL pad = 16, same width. c-ares opens via strlen so the NULs\n\
         # don't matter at the syscall layer. Bun-compiled CLIs whose\n\
         # static-musl libc bypasses LD_PRELOAD now read our DNS file\n\
         # instead of the missing /etc/resolv.conf — proot wrap dropped.\n\
         patch_resolv_conf() {{\n    \
             bin=\"$1\"\n    \
             [ -x \"$PREFIX/bin/perl\" ] || return 0\n    \
             grep -q -a -- '/etc/resolv.conf' \"$bin\" 2>/dev/null || return 0\n    \
             \"$PREFIX/bin/perl\" -e '\n                my $path = $ARGV[0];\n                open my $fh, \"+<:raw\", $path or exit 0;\n                my $data = do {{ local $/; <$fh> }};\n                my $count = 0;\n                while ($data =~ /\\x00\\/etc\\/resolv\\.conf\\x00/g) {{\n                    my $offset = $-[0] + 1;\n                    seek $fh, $offset, 0;\n                    print $fh \"/sdcard/.zed/r\\x00\\x00\";\n                    $count++;\n                }}\n                close $fh;\n                print STDERR \"zed-launcher-gen: hex-patched $count /etc/resolv.conf in $path\\n\" if $count > 0;\n            ' \"$bin\" 2>&1\n\
         }}\n\
         \n\
         handle_elf() {{\n    \
             bin=\"$1\"\n    \
             # Skip shell wrappers (ours from a previous run, or otherwise).\n    \
             head -c 2 -- \"$bin\" 2>/dev/null | grep -q '#!' && return 0\n    \
             # Skip non-ELF (JSON, .so without exec bit, etc.).\n    \
             \"$PREFIX/bin/readelf\" -h \"$bin\" >/dev/null 2>&1 || return 0\n    \
             patch_musl_interp \"$bin\"\n    \
             patch_resolv_conf \"$bin\"\n    \
             # For musl-static binaries: write a tiny wrapper at\n    \
             # $PREFIX/bin/<basename> that strips LD_PRELOAD before exec.\n    \
             # Reason: lib.rs sets LD_PRELOAD=libtermux-exec.so for the\n    \
             # whole process tree (bash/Node/etc inherit it for shebang\n    \
             # path-rewriting on com.termux paths). libtermux-exec is\n    \
             # bionic-linked; loading it into a musl process fails on\n    \
             # __system_property_get, __register_atfork, FORTIFY _chk\n    \
             # symbols that musl doesn't provide. The wrapper invokes the\n    \
             # patched binary directly (skipping any JS dispatcher) so\n    \
             # the env strip applies to the musl process from its first\n    \
             # exec — same shape as the old proot wrapper had `env -u\n    \
             # LD_PRELOAD` for the same reason, just minus the proot.\n    \
             interp=$(\"$PREFIX/bin/readelf\" -l \"$bin\" 2>/dev/null | awk '/interpreter:/ {{ gsub(/[\\[\\]]/, \"\", $NF); print $NF; exit }}')\n    \
             case \"$interp\" in\n        \
                 \"$PREFIX/lib/ld-musl-aarch64.so.1\")\n            \
                     name=$(basename -- \"$bin\")\n            \
                     want=\"#!$PREFIX/bin/sh\n\
exec env -u LD_PRELOAD \\\"$bin\\\" \\\"\\$@\\\"\"\n            \
                     write_if_changed \"$PREFIX/bin/$name\" \"$want\"\n            \
                     ;;\n    \
             esac\n\
         }}\n\
         \n\
         handle_link() {{\n    \
             link=\"$1\"\n    \
             target=$(readlink -f -- \"$link\" 2>/dev/null)\n    \
             [ -n \"$target\" ] && [ -f \"$target\" ] || return 0\n    \
             case \"$target\" in\n        \
                 \"$PREFIX\"/lib/node_modules/*) ;;\n        \
                 *) return 0 ;;\n    \
             esac\n    \
             handle_elf \"$target\"\n    \
             # Glibc-dynamic targets need a wrapper through grun (or a\n    \
             # stub if grun isn't installed). musl/static targets are\n    \
             # already in-place patched above; npm's symlink invokes them\n    \
             # directly, kernel reads the patched ELF, c-ares reads\n    \
             # /sdcard/.zed/r, no extra wrapper needed.\n    \
             interp=$(\"$PREFIX/bin/readelf\" -l \"$target\" 2>/dev/null | awk '/interpreter:/ {{ gsub(/[\\[\\]]/, \"\", $NF); print $NF; exit }}')\n    \
             case \"$interp\" in\n        \
                 */ld-linux-*)\n            \
                     name=$(basename -- \"$link\")\n            \
                     if [ -x \"$PREFIX/bin/grun\" ]; then\n                \
                         want=\"#!$PREFIX/bin/sh\n\
exec \\\"$PREFIX/bin/grun\\\" \\\"$target\\\" \\\"\\$@\\\"\"\n            \
                     else\n                \
                         want=\"#!$PREFIX/bin/sh\n\
echo \\\"error: $name needs glibc-runner. Install via:\\\" >&2\n\
echo \\\"  pkg install tur-repo && pkg install glibc-runner\\\" >&2\n\
exit 1\"\n            \
                     fi\n            \
                     write_if_changed \"$link\" \"$want\"\n            \
                     ;;\n    \
             esac\n\
         }}\n\
         \n\
         for link in \"$PREFIX\"/bin/*; do\n    \
             [ -L \"$link\" ] || continue\n    \
             handle_link \"$link\"\n\
         done\n\
         \n\
         find \"$PREFIX/lib/node_modules\" -type f -perm -u+x \\\n             ! -name '*.js' ! -name '*.cjs' ! -name '*.mjs' ! -name '*.json' \\\n             ! -name 'node' ! -name 'corepack' \\\n             ! -name '*.real' \\\n             2>/dev/null | while IFS= read -r bin; do\n    \
             handle_elf \"$bin\"\n\
         done\n\
         exit 0\n"
    );

    let helper_path = etc_apt.join("zed-launcher-gen.sh");
    let helper_changed = match std::fs::read(&helper_path) {
        Ok(existing) => existing != body.as_bytes(),
        Err(_) => true,
    };
    if !helper_changed {
        return Ok(());
    }
    std::fs::write(&helper_path, body.as_bytes())
        .with_context(|| format!("write {}", helper_path.display()))?;
    let mut perms = std::fs::metadata(&helper_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&helper_path, perms)?;
    log::info!(
        "termux_bootstrap: wrote launcher generator at {}",
        helper_path.display()
    );
    Ok(())
}

/// Rewrite NODE_PLATFORM string from "android" to "linux\0\0" inside
/// `$PREFIX/bin/node` so `process.platform === 'linux'` at runtime.
///
/// Why: Termux compiles Node.js with `--dest-os=android`, which bakes
/// the literal `"android"` into the binary's .rodata. Node reads it via
/// `OneByteString(isolate, NODE_PLATFORM)` whose strlen-based length
/// computation makes downstream behavior fully runtime-driven. By
/// replacing the 7-byte literal with `"linux\0\0"` (5 chars + 2 NUL
/// padding to keep the byte count fixed), strlen returns 5 → V8
/// creates a 5-byte JS string `"linux"` → every `process.platform`
/// check in npm / node packages / claude-code / codex / etc. takes the
/// Linux branch. No NODE_OPTIONS shim, no per-package wrappers.
///
/// Targeting the standalone null-bounded `\0android\0` pattern is safe:
/// only NODE_PLATFORM appears that way; the other "android" substrings
/// in the binary (e.g. `com.android.tzdata`, `highend_android_phys`,
/// some V8 error strings) are part of larger tokens and don't match.
///
/// One-shot at boot (this function); also wired as a DPkg::Post-Invoke
/// hook so future `pkg install nodejs` / `pkg upgrade` automatically
/// re-applies the patch — see `install_apt_node_platform_hook`.
fn patch_node_platform_now(prefix: &Path) {
    let node_bin = prefix.join("bin/node");
    if !node_bin.is_file() {
        return; // nodejs not installed yet
    }
    let mut data = match std::fs::read(&node_bin) {
        Ok(d) => d,
        Err(err) => {
            log::warn!(
                "termux_bootstrap: read {} failed: {err:#}",
                node_bin.display()
            );
            return;
        }
    };
    // Window-search for the standalone null-bounded literal. We scan the
    // whole 46MB binary once; on already-patched binaries we just don't
    // find a match and bail in milliseconds.
    let needle: &[u8] = b"\x00android\x00";
    let pos = match data.windows(needle.len()).position(|w| w == needle) {
        Some(p) => p,
        None => return, // already patched (or no NODE_PLATFORM match)
    };
    let target = pos + 1;
    let replacement: &[u8] = b"linux\x00\x00";
    data[target..target + replacement.len()].copy_from_slice(replacement);
    if let Err(err) = std::fs::write(&node_bin, &data) {
        log::warn!(
            "termux_bootstrap: write {} failed: {err:#}",
            node_bin.display()
        );
        return;
    }
    log::info!(
        "termux_bootstrap: patched NODE_PLATFORM literal at offset {target} \
         in {} (process.platform now reports 'linux')",
        node_bin.display()
    );
}

/// `DPkg::Post-Invoke` hook that re-runs the same NODE_PLATFORM rewrite
/// after every dpkg invocation, so `pkg install nodejs` / `pkg upgrade
/// nodejs` automatically produce a Linux-reporting Node binary without
/// requiring an app relaunch. Idempotent — the helper script no-ops on
/// already-patched binaries.
///
/// Implementation uses `perl` (shipped in our bootstrap) to handle the
/// binary-safe pattern match + write; raw `grep -P` works for finding
/// but not for the in-place rewrite, and we'd rather one tool than
/// piping through `dd` with arithmetic in shell.
fn install_apt_node_platform_hook(prefix: &Path) -> Result<()> {
    let conf_dir = prefix.join("etc/apt/apt.conf.d");
    if !conf_dir.is_dir() {
        return Ok(());
    }
    let etc_apt = prefix.join("etc/apt");
    if !etc_apt.is_dir() {
        return Ok(());
    }
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());

    let helper_path = etc_apt.join("zed-node-platform-hook.sh");
    let helper_body = format!(
        "#!{prefix_str}/bin/sh\n\
         # Auto-generated by gpui_android termux_bootstrap. Runs after\n\
         # every dpkg invocation; rewrites the NODE_PLATFORM string in\n\
         # $PREFIX/bin/node from 'android' to 'linux\\0\\0' so npm,\n\
         # claude, codex, and every other Node-mediated tool sees\n\
         # process.platform === 'linux'. Idempotent: scans for the\n\
         # standalone \\0android\\0 marker and exits cleanly when it's\n\
         # already been patched.\n\
         set -u\n\
         PREFIX={prefix_str}\n\
         NODE_BIN=\"$PREFIX/bin/node\"\n\
         [ -f \"$NODE_BIN\" ] || exit 0\n\
         [ -x \"$PREFIX/bin/perl\" ] || exit 0\n\
         \"$PREFIX/bin/perl\" -e '\n    \
             my $path = $ARGV[0];\n    \
             open my $fh, \"+<:raw\", $path or exit 0;\n    \
             my $data = do {{ local $/; <$fh> }};\n    \
             if ($data =~ /\\x00android\\x00/) {{\n        \
                 my $offset = $-[0] + 1;\n        \
                 seek $fh, $offset, 0;\n        \
                 print $fh \"linux\\x00\\x00\";\n        \
                 close $fh;\n        \
                 print STDERR \"zed-node-platform-hook: patched NODE_PLATFORM at offset $offset\\n\";\n    \
             }}\n\
         ' \"$NODE_BIN\" 2>&1\n\
         exit 0\n",
    );
    let helper_changed = match std::fs::read(&helper_path) {
        Ok(existing) => existing != helper_body.as_bytes(),
        Err(_) => true,
    };
    if helper_changed {
        std::fs::write(&helper_path, helper_body.as_bytes())
            .with_context(|| format!("write {}", helper_path.display()))?;
        let mut perms = std::fs::metadata(&helper_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&helper_path, perms)?;
        log::info!(
            "termux_bootstrap: wrote node-platform helper at {}",
            helper_path.display()
        );
    }

    let conf_path = conf_dir.join("97-zed-node-platform");
    let conf_body = format!(
        "// Auto-generated by gpui_android termux_bootstrap. Rewrites the\n\
         // NODE_PLATFORM literal in $PREFIX/bin/node after every dpkg\n\
         // invocation. See {helper_path}.\n\
         DPkg::Post-Invoke {{\n    \
             \"{helper_path} || true\";\n\
         }};\n",
        helper_path = helper_path.display(),
    );
    let conf_changed = match std::fs::read(&conf_path) {
        Ok(existing) => existing != conf_body.as_bytes(),
        Err(_) => true,
    };
    if conf_changed {
        std::fs::write(&conf_path, conf_body.as_bytes())
            .with_context(|| format!("write {}", conf_path.display()))?;
        log::info!(
            "termux_bootstrap: wrote apt node-platform hook at {}",
            conf_path.display()
        );
    }
    Ok(())
}

/// Closes the maintainer-script-shebang gap. Upstream Termux .debs ship
/// `preinst`/`postinst` scripts whose shebang line hardcodes
/// `#!/data/data/com.termux/files/usr/bin/<shell>`. dpkg extracts those
/// scripts to `lib/dpkg/tmp.ci/<script>` and execve's them; the kernel's
/// `binfmt_script` handler reads the shebang and tries to execve the
/// (nonexistent for our UID) com.termux path, returning EACCES.
///
/// LD_PRELOAD=libtermux-exec.so does NOT help — termux-exec hooks libc
/// `execve`, but shebang resolution is kernel-internal and bypasses
/// libc entirely.
///
/// Fix: apt's `DPkg::Pre-Install-Pkgs` hook fires BEFORE any dpkg call
/// with the list of incoming .deb file paths. We modify each .deb in
/// place by extracting it via `dpkg-deb -R`, sed-rewriting com.termux
/// references in `DEBIAN/{preinst,postinst,prerm,postrm}`, and rebuilding
/// it via `dpkg-deb -b`. The .deb that dpkg ultimately unpacks has
/// shebangs that point at our prefix, so the kernel's binfmt_script
/// handler succeeds.
///
/// Unlike the existing Post-Invoke `99-zed-rewrite-postinst` hook (which
/// fires after `dpkg --unpack` completes), this fires BEFORE — necessary
/// because preinst runs DURING --unpack, before any Post-Invoke fires.
fn install_apt_pre_install_hook(prefix: &Path) -> Result<()> {
    let conf_dir = prefix.join("etc/apt/apt.conf.d");
    if !conf_dir.is_dir() {
        return Ok(());
    }
    let etc_apt = prefix.join("etc/apt");
    if !etc_apt.is_dir() {
        return Ok(());
    }

    let helper_path = etc_apt.join("zed-pre-install-rewrite.sh");
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());
    // apt's Pre-Install-Pkgs protocol version 0: file paths arrive on
    // stdin, one per line. The first line is the protocol version
    // ("VERSION 2" or similar) — skip lines that don't end in `.deb`.
    let helper_body = format!(
        "#!{prefix_str}/bin/sh\n\
         # Auto-generated by gpui_android termux_bootstrap. apt's\n\
         # DPkg::Pre-Install-Pkgs invokes us with .deb paths on stdin\n\
         # before dpkg --unpack runs. We rewrite com.termux shebangs\n\
         # inside each .deb's maintainer scripts so the kernel's\n\
         # binfmt_script handler can resolve them.\n\
         set -u\n\
         PREFIX={prefix_str}\n\
         export PATH=\"$PREFIX/bin:$PATH\"\n\
         [ -x \"$PREFIX/bin/dpkg-deb\" ] || exit 0\n\
         while IFS= read -r line; do\n    \
             case \"$line\" in *.deb) ;; *) continue ;; esac\n    \
             deb=\"$line\"\n    \
             [ -f \"$deb\" ] || continue\n    \
             tmp=$(mktemp -d 2>/dev/null) || continue\n    \
             if \"$PREFIX/bin/dpkg-deb\" -R \"$deb\" \"$tmp\" 2>/dev/null; then\n        \
                 # grep -lI: list filenames containing the literal,\n        \
                 # skipping binary files (the -I flag). This catches\n        \
                 # both DEBIAN maintainer scripts AND data-archive\n        \
                 # scripts (npm, pip, helper scripts) whose shebangs\n        \
                 # point at /data/data/com.termux/...\n        \
                 matches=$(grep -rlI '/data/data/com\\.termux/' \"$tmp\" 2>/dev/null)\n        \
                 if [ -n \"$matches\" ]; then\n            \
                     printf '%s\\n' \"$matches\" | while IFS= read -r f; do\n                \
                         sed -i 's|/data/data/com\\.termux/|/data/data/com.zdroid/|g' \"$f\" 2>/dev/null\n            \
                     done\n            \
                     # dpkg-deb -R extracts maintainer scripts at 0644;\n            \
                     # -b refuses to rebuild unless they're 0555..0775.\n            \
                     for s in preinst postinst prerm postrm; do\n                \
                         [ -f \"$tmp/DEBIAN/$s\" ] && chmod 0755 \"$tmp/DEBIAN/$s\" 2>/dev/null\n            \
                     done\n            \
                     \"$PREFIX/bin/dpkg-deb\" -b \"$tmp\" \"$deb\" >/dev/null 2>&1 || true\n        \
                 fi\n    \
             fi\n    \
             rm -rf \"$tmp\"\n\
         done\n\
         exit 0\n",
    );
    let helper_changed = match std::fs::read(&helper_path) {
        Ok(existing) => existing != helper_body.as_bytes(),
        Err(_) => true,
    };
    if helper_changed {
        std::fs::write(&helper_path, helper_body.as_bytes())
            .with_context(|| format!("write {}", helper_path.display()))?;
        let mut perms = std::fs::metadata(&helper_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&helper_path, perms)?;
        log::info!(
            "termux_bootstrap: wrote pre-install rewrite helper at {}",
            helper_path.display()
        );
    }

    let conf_path = conf_dir.join("97-zed-pre-install");
    let conf_body = format!(
        "// Auto-generated by gpui_android termux_bootstrap. Pre-Install-\n\
         // Pkgs hook: rewrites com.termux shebangs in incoming .debs'\n\
         // maintainer scripts before dpkg --unpack runs them. See\n\
         // {helper_path}.\n\
         DPkg::Pre-Install-Pkgs {{\n    \
             \"{helper_path}\";\n\
         }};\n",
        helper_path = helper_path.display(),
    );
    let conf_changed = match std::fs::read(&conf_path) {
        Ok(existing) => existing != conf_body.as_bytes(),
        Err(_) => true,
    };
    if conf_changed {
        std::fs::write(&conf_path, conf_body.as_bytes())
            .with_context(|| format!("write {}", conf_path.display()))?;
        log::info!(
            "termux_bootstrap: wrote apt Pre-Install-Pkgs hook at {}",
            conf_path.display()
        );
    }
    Ok(())
}

/// Pin dpkg + dpkg-deb against any upstream upgrade. Without this, an
/// `apt install dpkg` (or any apt --fix-broken install where dpkg is
/// in the dep closure) replaces our patched binaries with upstream's,
/// which have `com.termux` baked into RUNPATH and many other string
/// constants that aren't fixable by env vars or LD_PRELOAD. Recovery
/// is full bootstrap re-extract — costly. The pin avoids the recovery.
///
/// Pin-Priority 1001 means apt holds the currently-installed version
/// even against same-version upstream replacements (the empirical
/// failure mode: same 1.22.6-5, but different binary content).
fn install_apt_dpkg_pin(prefix: &Path) -> Result<()> {
    let pref_dir = prefix.join("etc/apt/preferences.d");
    if !pref_dir.is_dir() {
        return Ok(());
    }
    let pref_path = pref_dir.join("zed-pin-dpkg");
    // NOTE: apt_preferences(5) uses `#` for comments, NOT `//` like
    // apt.conf(5). Keep the comment style on the right side of that
    // boundary — apt rejects `//`-prefixed lines as "no Package header".
    let body = "# Auto-generated by gpui_android termux_bootstrap. The patched\n\
                # dpkg + dpkg-deb shipped in our bootstrap have `com.termux` →\n\
                # our-package-name path rewriting in lib/dpkg/tarfn.c and\n\
                # src/deb/extract.c. Upstream's binaries don't, and replacing\n\
                # ours with theirs bricks the whole package manager (RUNPATH,\n\
                # sysconfdir, info-dir all baked to com.termux). Forbid the\n\
                # upgrade.\n\
                Package: dpkg\n\
                Pin: release *\n\
                Pin-Priority: 1001\n";
    let needs_write = match std::fs::read(&pref_path) {
        Ok(existing) => existing != body.as_bytes(),
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&pref_path, body)
            .with_context(|| format!("write {}", pref_path.display()))?;
        log::info!(
            "termux_bootstrap: wrote apt dpkg pin at {}",
            pref_path.display()
        );
    }
    Ok(())
}

/// Rewrite `/data/data/com.termux/` → `/data/data/com.zdroid/` in every
/// dpkg metadata file inside `$PREFIX/var/lib/dpkg/info/` plus the master
/// `status` file. The covered suffixes:
///
///   - **maintainer scripts** (`preinst`, `postinst`, `prerm`, `postrm`) —
///     execve'd directly by dpkg --configure; baked-in com.termux paths
///     would invoke binaries in the OTHER app's sandbox and EACCES
///   - **`conffiles`** — newline-separated list of paths dpkg treats as
///     editable config files. Read at --configure time to decide which
///     files are conffiles vs regular data; baked-in com.termux paths
///     make dpkg look at the wrong (cross-app) sandbox and the configure
///     fails with `unable to stat new distributed configfile … Permission
///     denied` — exactly the glibc-termux failure that bit us 2026-05-06
///   - **`md5sums`** — `<hash> <path>` per line; consulted by `dpkg
///     --verify` and during conffile-conflict resolution. Mismatched
///     paths confuse upgrades and `dpkg --audit`
///   - **`list`** — newline-separated list of every file the package
///     installed. dpkg --remove walks this list to delete files; baked-in
///     com.termux paths make it walk a non-existent (or cross-app) tree
///   - **`triggers`** / **`templates`** — less load-bearing but symmetric
///     for completeness (templates is debconf, triggers is for inter-pkg
///     deferred actions)
///
/// Plus `$PREFIX/var/lib/dpkg/status` (the master DB), which can also
/// have com.termux paths from old bootstrap state or upstream metadata
/// that leaked through. `Homepage:` URLs in status legitimately contain
/// "termux" (project name) and stay — only `/data/data/com.termux/`
/// path patterns are rewritten, so URLs aren't touched.
///
/// All rewrites are exact-length byte substitution (22 ↔ 22) — the
/// load-bearing property of the `com.zdroid` rename. No length games,
/// no NUL pad, in-place safe even on huge `status` files.
fn rewrite_maintainer_scripts(info_dir: &Path) -> Result<()> {
    if !info_dir.is_dir() {
        // Bootstrap not extracted yet, or already wiped — caller already
        // logged. Don't error from a no-op.
        return Ok(());
    }
    let entries = std::fs::read_dir(info_dir)
        .with_context(|| format!("read_dir {}", info_dir.display()))?;
    let mut rewritten = 0usize;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let suffix = match path.extension().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if !matches!(
            suffix,
            "preinst"
                | "postinst"
                | "prerm"
                | "postrm"
                | "conffiles"
                | "md5sums"
                | "list"
                | "triggers"
                | "templates"
        ) {
            continue;
        }
        if rewrite_one_script(&path)? {
            rewritten += 1;
        }
    }
    // Master dpkg state DB — paths can land here directly from .deb
    // metadata (Description-Md5, Conffiles: blocks, etc.). Same sed shape.
    let status_path = info_dir
        .parent()
        .map(|p| p.join("status"))
        .unwrap_or_else(|| info_dir.join("../status"));
    if status_path.is_file() && rewrite_one_script(&status_path)? {
        rewritten += 1;
    }
    if rewritten > 0 {
        log::info!(
            "termux_bootstrap: rewrote com.termux paths in {} dpkg metadata file(s)",
            rewritten
        );
    }
    Ok(())
}

fn rewrite_one_script(path: &Path) -> Result<bool> {
    const NEEDLE: &[u8] = b"/data/data/com.termux/";
    const REPLACEMENT: &[u8] = b"/data/data/com.zdroid/";
    let content = std::fs::read(path)
        .with_context(|| format!("read {}", path.display()))?;
    if !content.windows(NEEDLE.len()).any(|w| w == NEEDLE) {
        return Ok(false);
    }
    let mut out = Vec::with_capacity(content.len() + 64);
    let mut i = 0;
    while i < content.len() {
        if content[i..].starts_with(NEEDLE) {
            out.extend_from_slice(REPLACEMENT);
            i += NEEDLE.len();
        } else {
            out.push(content[i]);
            i += 1;
        }
    }
    std::fs::write(path, &out)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

fn install_apt_rewrite_hook(prefix: &Path) -> Result<()> {
    let conf_dir = prefix.join("etc/apt/apt.conf.d");
    if !conf_dir.is_dir() {
        return Ok(());
    }
    let conf_path = conf_dir.join("99-zed-rewrite-postinst");
    let info = prefix.join("var/lib/dpkg/info");
    let status = prefix.join("var/lib/dpkg/status");
    // Single sed via shell glob over EVERY dpkg metadata file type that
    // can carry baked-in com.termux paths, plus the master status DB.
    // `\\.` (single backslash + dot) is what apt's quoted-string parser
    // emits; the running shell sees `\.` and sed sees a literal `.`.
    // `2>/dev/null || true` keeps the hook silent and never fails apt
    // when a glob matches zero files (e.g. between unpack of a deb
    // with no scripts and the next configure pass).
    //
    // Coverage rationale (learned 2026-05-06 when glibc-termux install
    // failed at --configure with `unable to stat new distributed
    // configfile '/data/data/com.termux/files/usr/glibc/etc/gai.conf
    // .dpkg-new': Permission denied`):
    //   *.preinst/postinst/prerm/postrm  — execve'd by --configure
    //   *.conffiles                     — read by --configure to decide
    //                                      conffile vs data; the gap
    //                                      that bit us
    //   *.md5sums                       — read by --verify, --audit
    //   *.list                          — walked by --remove
    //   *.triggers / *.templates        — symmetric for completeness
    //   $PREFIX/var/lib/dpkg/status     — master DB; paths can land here
    //                                      from Conffiles: blocks etc.
    let body = format!(
        "// Auto-generated by gpui_android termux_bootstrap. Bridges the\n\
         // dpkg path-rewrite patches: rewrites com.termux references in\n\
         // EVERY dpkg metadata file type plus the master status DB after\n\
         // every dpkg --unpack so --configure runs against clean state.\n\
         // 22==22 byte substitution (com.termux <-> com.zdroid) — the\n\
         // load-bearing equal-length property of the com.zdroid rename.\n\
         DPkg::Post-Invoke {{\n    \
             \"sed -i 's|/data/data/com\\\\.termux/|/data/data/com.zdroid/|g' \
              {info}/*.preinst {info}/*.postinst {info}/*.prerm {info}/*.postrm \
              {info}/*.conffiles {info}/*.md5sums {info}/*.list \
              {info}/*.triggers {info}/*.templates {status} \
              2>/dev/null || true\";\n\
         }};\n",
        info = info.display(),
        status = status.display(),
    );
    let needs_write = match std::fs::read(&conf_path) {
        Ok(existing) => existing != body.as_bytes(),
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&conf_path, body.as_bytes())
            .with_context(|| format!("write {}", conf_path.display()))?;
        log::info!(
            "termux_bootstrap: wrote apt rewrite hook at {}",
            conf_path.display()
        );
    }
    Ok(())
}

/// Closes the layer-3 gap: upstream Termux's CI bakes
/// `/data/data/com.termux/files/usr/...` into every package's ELF
/// `DT_RUNPATH` *and* `.rodata` string constants. Without rewriting
/// both, every freshly `pkg install`'d binary fails with either
/// `library "libfoo.so" not found` (RUNPATH) or `open(...) ENOENT`
/// at runtime (rodata). Layer 1 (our dpkg patches) puts files at the
/// right path; this hook makes those files actually runnable.
///
/// Mechanism: `DPkg::Post-Invoke` apt hook fires after every dpkg
/// invocation. The hook calls a shell helper that walks
/// `$PREFIX/{bin,sbin,libexec}` plus `$PREFIX/lib/*.so*`, filtered
/// to files with status-changed time under 1 minute, and applies two
/// passes per ELF:
///
///   - `patchelf --set-rpath $PREFIX/lib` (skips files already
///     correct; never `--force-rpath` since that converts
///     `DT_RUNPATH` → `DT_RPATH` and corrupts certain libs).
///   - In-place hex-substitution `/data/data/com.termux/` →
///     `/data/data/com.zdroid/` via Perl `+<:raw` open + regex
///     match-and-overwrite. Both prefixes are exactly 22 bytes so
///     the rewrite is byte-for-byte: no length games, no NUL pad,
///     no PT_LOAD reflow.
///
/// The hex-substitution is idempotent — after the first run the
/// `com.termux` needle no longer exists in the file, so subsequent
/// `apt install` invocations are no-ops on already-patched files.
/// The 22 == 22 invariant is the load-bearing structural property
/// of the `com.zdroid` rename: pick any applicationId of a different
/// length and this whole class of fix collapses (back to LD_PRELOAD
/// shims and per-package wrappers).
///
/// Requires `patchelf` and `perl` in the bootstrap (both shipped via
/// `build-bootstraps.sh --add` invocations on the Vultr rebuild
/// instance). The helper script no-ops gracefully if either is
/// missing.
fn install_apt_patchelf_hook(prefix: &Path) -> Result<()> {
    let conf_dir = prefix.join("etc/apt/apt.conf.d");
    if !conf_dir.is_dir() {
        return Ok(());
    }
    let etc_apt = prefix.join("etc/apt");
    if !etc_apt.is_dir() {
        return Ok(());
    }

    // The shell helper. Inlined into apt.conf with all the escaping
    // would be a nightmare; a separate file is much cleaner.
    let helper_path = etc_apt.join("zed-patchelf-hook.sh");
    // Android's getFilesDir() returns the resolved "/data/user/0/<pkg>"
    // form. Termux's bootstrap binaries (built on Vultr with
    // TERMUX_APP_PACKAGE=...) bake the canonical "/data/data/<pkg>"
    // form into RUNPATH. The kernel/FS treats these as the same dir,
    // but the dynamic linker treats them as DIFFERENT namespaces. If
    // patchelf writes /data/user/0/... into a lib's RPATH, the linker
    // fails to find the lib's dependencies even though the file is
    // physically there. Canonicalize to /data/data/... to match the
    // bootstrap's convention.
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());
    let helper_body = format!(
        "#!{prefix_str}/bin/sh\n\
         # Auto-generated by gpui_android termux_bootstrap. Runs after\n\
         # every dpkg --unpack on freshly-installed binaries:\n\
         #   1. patchelf DT_RUNPATH -> $PREFIX/lib (Termux's CI bakes\n\
         #      /data/data/com.termux/files/usr/lib into every ELF).\n\
         #   2. hex-patch /data/data/com.termux/ -> /data/data/com.zdroid/\n\
         #      in rodata. Same 22-byte length so rewrite is in-place;\n\
         #      closes the layer-3 in-binary string-constant gap that\n\
         #      used to need LD_PRELOAD shims or per-package wrappers.\n\
         #      Idempotent — the needle is gone after run 1.\n\
         # Without these, the dynamic linker either can't resolve shared\n\
         # libs (RUNPATH) or the binary fails at first runtime path open\n\
         # (rodata).\n\
         #\n\
         # Critical correctness invariants (learned the hard way):\n\
         # 1. NO --force-rpath. It converts DT_RUNPATH->DT_RPATH and on\n\
         #    some libs (e.g. libandroid-support.so) corrupts the file\n\
         #    by truncating its dynamic section.\n\
         # 2. Skip files whose RUNPATH already matches our prefix.\n\
         #    Bootstrap libs (built on Vultr) are pre-correct; touching\n\
         #    them risks the same patchelf section-layout issues.\n\
         # 3. `-cmin -1` (status change time, NOT mtime). dpkg preserves\n\
         #    the .deb's mtime (typically the build time) when extracting,\n\
         #    so `-mmin` would skip just-installed files. ctime updates\n\
         #    when dpkg chmods/chowns the file post-extract, so it's\n\
         #    reliably \"recent\" for newly-installed files.\n\
         # 4. Hex-pattern length must equal substitute length, or it'd\n\
         #    shift every byte after the match (PT_LOAD/section table\n\
         #    offsets break, ELF unloadable). The com.zdroid applicationId\n\
         #    is precisely 10 chars to satisfy this against com.termux.\n\
         set -u\n\
         PREFIX={prefix_str}\n\
         WANT=\"$PREFIX/lib\"\n\
         [ -x \"$PREFIX/bin/patchelf\" ] || exit 0\n\
         maybe_patchelf() {{\n    \
             current=$(\"$PREFIX/bin/patchelf\" --print-rpath \"$1\" 2>/dev/null) || return 0\n    \
             [ \"$current\" = \"$WANT\" ] && return 0\n    \
             # If hex-patch already fixed the RUNPATH (com.zdroid present),\n    \
             # leave it alone — for glibc-stack libs the correct RUNPATH\n    \
             # is $PREFIX/glibc/lib, NOT $PREFIX/lib, and patchelf would\n    \
             # overwrite the hex-patch's correct value with the musl-stack\n    \
             # path. Hex-patch handled this binary; trust it.\n    \
             case \"$current\" in *com.zdroid*) return 0 ;; esac\n    \
             \"$PREFIX/bin/patchelf\" --set-rpath \"$WANT\" \"$1\" 2>/dev/null || true\n\
         }}\n\
         maybe_hex_patch() {{\n    \
             [ -x \"$PREFIX/bin/perl\" ] || return 0\n    \
             grep -q -a -- '/data/data/com.termux/' \"$1\" 2>/dev/null || return 0\n    \
             \"$PREFIX/bin/perl\" -e '\n                my $path = $ARGV[0];\n                open my $fh, \"+<:raw\", $path or exit 0;\n                my $data = do {{ local $/; <$fh> }};\n                my $count = 0;\n                while ($data =~ m{{/data/data/com\\.termux/}}g) {{\n                    my $offset = $-[0];\n                    seek $fh, $offset, 0;\n                    print $fh \"/data/data/com.zdroid/\";\n                    $count++;\n                }}\n                close $fh;\n                print STDERR \"zed-rodata-hex: $count com.termux -> com.zdroid in $path\\n\" if $count > 0;\n            ' \"$1\" 2>&1\n\
         }}\n\
         find \"$PREFIX/bin\" \"$PREFIX/sbin\" \"$PREFIX/libexec\" \
              \"$PREFIX/glibc/bin\" \"$PREFIX/glibc/sbin\" \"$PREFIX/glibc/libexec\" \
              -type f -cmin -10 2>/dev/null \
              | while IFS= read -r f; do maybe_hex_patch \"$f\"; maybe_patchelf \"$f\"; done\n\
         find \"$PREFIX/lib\" \"$PREFIX/glibc/lib\" \
              -type f -cmin -10 -name '*.so*' 2>/dev/null \
              | while IFS= read -r f; do maybe_hex_patch \"$f\"; maybe_patchelf \"$f\"; done\n\
         exit 0\n",
    );
    let helper_changed = match std::fs::read(&helper_path) {
        Ok(existing) => existing != helper_body.as_bytes(),
        Err(_) => true,
    };
    if helper_changed {
        std::fs::write(&helper_path, helper_body.as_bytes())
            .with_context(|| format!("write {}", helper_path.display()))?;
        let mut perms = std::fs::metadata(&helper_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&helper_path, perms)?;
        log::info!(
            "termux_bootstrap: wrote patchelf helper at {}",
            helper_path.display()
        );
    }

    // The apt config that calls the helper. Note `98-` prefix puts it
    // BEFORE `99-zed-rewrite-postinst` lexically, but DPkg::Post-Invoke
    // hooks fire in the order they're configured by apt, which doesn't
    // care about file order — both fire after the dpkg run, order
    // between them doesn't matter (sed touches scripts, patchelf
    // touches binaries; disjoint).
    let conf_path = conf_dir.join("98-zed-patchelf");
    let conf_body = format!(
        "// Auto-generated by gpui_android termux_bootstrap. Closes the\n\
         // layer-3 gap (upstream binary RUNPATH) by running patchelf on\n\
         // freshly-installed ELFs after each dpkg --unpack. See\n\
         // {helper_path}.\n\
         DPkg::Post-Invoke {{\n    \
             \"{helper_path} || true\";\n\
         }};\n",
        helper_path = helper_path.display(),
    );
    let conf_changed = match std::fs::read(&conf_path) {
        Ok(existing) => existing != conf_body.as_bytes(),
        Err(_) => true,
    };
    if conf_changed {
        std::fs::write(&conf_path, conf_body.as_bytes())
            .with_context(|| format!("write {}", conf_path.display()))?;
        log::info!(
            "termux_bootstrap: wrote apt patchelf hook at {}",
            conf_path.display()
        );
    }
    Ok(())
}

/// Copy `ld-musl-aarch64.so.1` from APK assets into `$PREFIX/lib` and
/// create the `libc.musl-aarch64.so.1` symlink (in musl, the dynamic
/// linker IS libc — same binary serves both DT_INTERP and DT_NEEDED
/// libc lookups). Extracted from Alpine's `musl-1.2.5-r23.apk` at
/// build time and shipped as a ~700KB asset; tiny enough to bundle
/// unconditionally and removes a manual extract step from the
/// claude-code-on-Android setup.
fn install_musl_linker(android_app: &AndroidApp, prefix: &Path) -> Result<()> {
    const ASSET: &str = "ld-musl-aarch64.so.1";
    let lib_dir = prefix.join("lib");
    std::fs::create_dir_all(&lib_dir)?;
    let target = lib_dir.join(ASSET);
    let alias = lib_dir.join("libc.musl-aarch64.so.1");

    let asset_manager = android_app.asset_manager();
    let asset_name = CString::new(ASSET)?;
    let mut asset = asset_manager
        .open(&asset_name)
        .ok_or_else(|| anyhow!("musl linker asset {ASSET} missing from APK"))?;
    let mut bytes = Vec::with_capacity(asset.length());
    asset.read_to_end(&mut bytes)?;

    std::fs::write(&target, &bytes)
        .with_context(|| format!("write {}", target.display()))?;
    let mut perms = std::fs::metadata(&target)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&target, perms)?;

    // Replace any existing alias (e.g. from a previous extract) so the
    // symlink always points at the current linker file.
    if alias.exists() || alias.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&alias);
    }
    std::os::unix::fs::symlink(ASSET, &alias)
        .with_context(|| format!("symlink {} -> {ASSET}", alias.display()))?;

    log::info!(
        "termux_bootstrap: installed musl linker ({} bytes) at {}",
        bytes.len(),
        target.display()
    );
    Ok(())
}

fn replay_symlinks(staging: &Path, symlinks: &[(String, String)]) -> Result<()> {
    for (target, link_rel) in symlinks {
        let link_rel = link_rel.trim_start_matches("./");
        let link_abs = staging.join(link_rel);
        if let Some(parent) = link_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if link_abs.exists() || link_abs.symlink_metadata().is_ok() {
            // Pre-existing entry from a previous staged run; replace.
            let _ = std::fs::remove_file(&link_abs);
        }
        std::os::unix::fs::symlink(target, &link_abs).with_context(|| {
            format!("symlink {} -> {}", link_abs.display(), target)
        })?;
    }
    Ok(())
}

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
