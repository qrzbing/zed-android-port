//! First-launch extractor for the bundled Termux bootstrap.
//!
//! 1:1 port of Termux's `TermuxInstaller.java`. The bundled bootstrap zip
//! lives in `assets/bootstrap-aarch64.zip`, manually SCP'd from the Vultr
//! build host after each rebuild — there is no Gradle download task yet.
//! Every binary inside the bootstrap has
//! `/data/data/dev.zed.zed_android/files/usr/...` baked into its
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
const BOOTSTRAP_VERSION: &str = "2026.04.26-r1+apt.android-7-zed-r11";

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
/// at /data/data/dev.zed.zed_android/... instead of /data/data/com.termux/...),
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
    patch_node_platform_now(&prefix);
    install_npm_launcher_generator(&prefix)?;
    install_npm_wrapper(&prefix)?;
    install_claude_setup_script(&prefix)?;
    auto_fix_claude_if_broken(&prefix);
    Ok(())
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
         # Auto-generated by gpui_android termux_bootstrap. Walks\n\
         # $PREFIX/bin/ symlinks pointing into $PREFIX/lib/node_modules/,\n\
         # classifies each target by ELF interpreter + hardcoded-path scan,\n\
         # and emits the right runtime wrapper:\n\
         #   - static or musl-dynamic, hardcoded /etc/resolv.conf -> proot\n\
         #   - musl-dynamic, no hardcode -> patchelf'd, npm symlink kept\n\
         #   - glibc-dynamic -> grun wrapper if installed, else stub\n\
         #   - everything else -> npm's symlink kept\n\
         # Idempotent: same target -> same wrapper body, content compare.\n\
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
         classify_and_wrap() {{\n    \
             link=\"$1\"\n    \
             target=$(readlink -f -- \"$link\" 2>/dev/null)\n    \
             [ -n \"$target\" ] && [ -f \"$target\" ] || return 0\n    \
             case \"$target\" in\n        \
                 \"$PREFIX\"/lib/node_modules/*) ;;\n        \
                 *) return 0 ;;\n    \
             esac\n    \
             # Skip non-ELF (scripts, JSON, etc.).\n    \
             \"$PREFIX/bin/readelf\" -h -- \"$target\" >/dev/null 2>&1 || return 0\n    \
             interp=$(\"$PREFIX/bin/readelf\" -l -- \"$target\" 2>/dev/null \\\n                 | awk '/interpreter:/ {{ gsub(/[\\[\\]]/, \"\", $NF); print $NF; exit }}')\n    \
             # Repoint musl interpreter if it points at the canonical /lib\n    \
             # path (Android has no /lib; the linker actually lives at\n    \
             # $PREFIX/lib/ld-musl-aarch64.so.1).\n    \
             case \"$interp\" in\n        \
                 /lib/ld-musl-aarch64.so.1)\n            \
                     if [ -x \"$PREFIX/bin/patchelf\" ]; then\n                \
                         \"$PREFIX/bin/patchelf\" --set-interpreter \\\n                     \"$PREFIX/lib/ld-musl-aarch64.so.1\" -- \"$target\" 2>/dev/null || true\n                \
                         \"$PREFIX/bin/patchelf\" --set-rpath \\\n                     \"$PREFIX/lib\" -- \"$target\" 2>/dev/null || true\n                \
                         interp=\"$PREFIX/lib/ld-musl-aarch64.so.1\"\n            \
                     fi\n            \
                     ;;\n    \
             esac\n    \
             \n    \
             needs_proot=0\n    \
             needs_grun=0\n    \
             case \"$interp\" in\n        \
                 */ld-linux-*)\n            \
                     needs_grun=1\n            \
                     ;;\n        \
                 *)\n            \
                     # Static or musl-dynamic. Check for hardcoded paths\n            \
                     # that LD_PRELOAD can't intercept (statically linked\n            \
                     # syscalls don't go through PLT/GOT).\n            \
                     if grep -q -a -- '/etc/resolv.conf' \"$target\" 2>/dev/null; then\n                \
                         needs_proot=1\n            \
                     fi\n            \
                     ;;\n    \
             esac\n    \
             \n    \
             if [ \"$needs_proot\" = \"1\" ]; then\n        \
                 if [ ! -x \"$PREFIX/bin/proot\" ]; then\n            \
                     return 0  # proot missing; leave npm's symlink alone\n        \
                 fi\n        \
                 want=\"#!$PREFIX/bin/sh\n\
exec env -u LD_PRELOAD \\\"$PREFIX/bin/proot\\\" -b \\\"$PREFIX/etc/resolv.conf:/etc/resolv.conf\\\" \\\"$target\\\" \\\"\\$@\\\"\"\n        \
                 write_if_changed \"$link\" \"$want\"\n    \
             elif [ \"$needs_grun\" = \"1\" ]; then\n        \
                 if [ -x \"$PREFIX/bin/grun\" ]; then\n            \
                     want=\"#!$PREFIX/bin/sh\n\
exec \\\"$PREFIX/bin/grun\\\" \\\"$target\\\" \\\"\\$@\\\"\"\n        \
                 else\n            \
                     name=$(basename -- \"$link\")\n            \
                     want=\"#!$PREFIX/bin/sh\n\
echo \\\"error: $name needs glibc-runner. Install via:\\\" >&2\n\
echo \\\"  pkg install tur-repo && pkg install glibc-runner\\\" >&2\n\
exit 1\"\n        \
                 fi\n        \
                 write_if_changed \"$link\" \"$want\"\n    \
             fi\n\
         }}\n\
         \n\
         # Deep-walk node_modules for ELFs not exposed via $PREFIX/bin/.\n\
         # Many npm packages (claude-code, codex, …) ship a JS dispatch\n\
         # stub at bin/<name> and the actual native binary deep inside\n\
         # an optional-dep package directory. The JS shim spawns the\n\
         # binary directly — never going through $PREFIX/bin/ — so the\n\
         # symlink-walking pass above misses it. Deep-walk applies the\n\
         # same classification and (for binaries needing proot) rewrites\n\
         # the binary in place: rename real binary to <name>.real, write\n\
         # a shell wrapper at the original path that proot-execs the\n\
         # .real. Idempotent — won't re-rename a wrapper we already wrote.\n\
         wrap_inplace_if_needed() {{\n    \
             bin=\"$1\"\n    \
             # Skip wrappers we already generated.\n    \
             head -c 2 -- \"$bin\" 2>/dev/null | grep -q '#!' && return 0\n    \
             # Must be an ELF.\n    \
             \"$PREFIX/bin/readelf\" -h -- \"$bin\" >/dev/null 2>&1 || return 0\n    \
             interp=$(\"$PREFIX/bin/readelf\" -l -- \"$bin\" 2>/dev/null \\\n                 | awk '/interpreter:/ {{ gsub(/[\\[\\]]/, \"\", $NF); print $NF; exit }}')\n    \
             # Repoint musl interpreter inline (same as classify_and_wrap).\n    \
             case \"$interp\" in\n        \
                 /lib/ld-musl-aarch64.so.1)\n            \
                     if [ -x \"$PREFIX/bin/patchelf\" ]; then\n                \
                         \"$PREFIX/bin/patchelf\" --set-interpreter \\\n                     \"$PREFIX/lib/ld-musl-aarch64.so.1\" -- \"$bin\" 2>/dev/null || true\n                \
                         \"$PREFIX/bin/patchelf\" --set-rpath \\\n                     \"$PREFIX/lib\" -- \"$bin\" 2>/dev/null || true\n            \
                     fi\n            \
                     ;;\n    \
             esac\n    \
             # Only proceed with wrap if the binary hardcodes a path we\n    \
             # can't reach via LD_PRELOAD on a static binary.\n    \
             grep -q -a -- '/etc/resolv.conf' \"$bin\" 2>/dev/null || return 0\n    \
             [ -x \"$PREFIX/bin/proot\" ] || return 0\n    \
             real=\"$bin.real\"\n    \
             if [ ! -f \"$real\" ]; then\n        \
                 mv -- \"$bin\" \"$real\" || return 0\n    \
             fi\n    \
             want=\"#!$PREFIX/bin/sh\n\
# Auto-generated by zed-launcher-gen. Wraps a deep node_modules ELF\n\
# whose static-musl libc bypass-syscalls hardcoded /etc/resolv.conf.\n\
exec env -u LD_PRELOAD \\\"$PREFIX/bin/proot\\\" -b \\\"$PREFIX/etc/resolv.conf:/etc/resolv.conf\\\" \\\"$real\\\" \\\"\\$@\\\"\"\n    \
             write_if_changed \"$bin\" \"$want\"\n\
         }}\n\
         \n\
         for link in \"$PREFIX\"/bin/*; do\n    \
             [ -L \"$link\" ] || continue\n    \
             classify_and_wrap \"$link\"\n\
         done\n\
         \n\
         # find -executable picks ELFs and shebang scripts; wrap_inplace\n\
         # filters scripts via head -c 2 + readelf checks. Skipping by\n\
         # name patterns to avoid touching node, node-gyp, npm internals.\n\
         find \"$PREFIX/lib/node_modules\" -type f -perm -u+x \\\n             ! -name '*.js' ! -name '*.cjs' ! -name '*.mjs' ! -name '*.json' \\\n             ! -name 'node' ! -name 'corepack' \\\n             2>/dev/null | while IFS= read -r bin; do\n    \
             wrap_inplace_if_needed \"$bin\"\n\
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

/// User-facing automation: if `npm install -g @anthropic-ai/claude-code`
/// has happened (the package directory exists) but the claude.exe is
/// still the small JS stub that prints "native binary not installed",
/// run `zed-setup-claude` automatically so the user doesn't have to.
///
/// Detection heuristic: claude.exe < 1MB → JS stub; >= 1MB → real ELF
/// (the smallest real Bun-compiled binary we've seen is ~240MB; a
/// stub is ~10KB). Cheap stat call, no fork.
///
/// Failures here are ignored — the script will print its own errors
/// the next time the user invokes claude, and worst case they can
/// run `zed-setup-claude` manually.
fn auto_fix_claude_if_broken(prefix: &Path) {
    let claude_exe = prefix.join("lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe");
    let metadata = match std::fs::metadata(&claude_exe) {
        Ok(m) => m,
        Err(_) => return, // claude-code not installed; nothing to fix
    };
    if metadata.len() >= 1_000_000 {
        return; // already a real binary, properly set up
    }
    log::info!(
        "termux_bootstrap: claude-code installed but unconfigured \
         ({}KB stub at {}); running zed-setup-claude in background",
        metadata.len() / 1024,
        claude_exe.display()
    );
    let setup_path = prefix.join("bin/zed-setup-claude");
    if !setup_path.exists() {
        log::warn!(
            "termux_bootstrap: {} missing; can't auto-fix claude",
            setup_path.display()
        );
        return;
    }
    // Spawn detached so app boot isn't blocked on npm install — claude
    // setup can take 30+ seconds and we'd hold up the editor that long.
    // The user will see the result the next time they run `claude`
    // (which by then will exec the real binary instead of the stub).
    let prefix_owned = prefix.to_owned();
    std::thread::Builder::new()
        .name("zed-setup-claude-bg".into())
        .spawn(move || {
            let bin = prefix_owned.join("bin");
            let path_env = std::env::var("PATH").unwrap_or_default();
            let path_with_prefix = format!("{}:{}", bin.display(), path_env);
            let result = std::process::Command::new(setup_path)
                .env("PATH", &path_with_prefix)
                .env("HOME", prefix_owned.parent().unwrap_or(&prefix_owned).join("home"))
                .env("PREFIX", &prefix_owned)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output();
            match result {
                Ok(out) if out.status.success() => log::info!(
                    "termux_bootstrap: claude auto-setup succeeded"
                ),
                Ok(out) => log::warn!(
                    "termux_bootstrap: claude auto-setup exited {} — \
                     stderr tail: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .join(" / "),
                ),
                Err(err) => log::warn!(
                    "termux_bootstrap: claude auto-setup spawn failed: {err:#}"
                ),
            }
        })
        .ok();
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
                         sed -i 's|/data/data/com\\.termux/|/data/data/dev.zed.zed_android/|g' \"$f\" 2>/dev/null\n            \
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
        if !matches!(suffix, "preinst" | "postinst" | "prerm" | "postrm") {
            continue;
        }
        if rewrite_one_script(&path)? {
            rewritten += 1;
        }
    }
    if rewritten > 0 {
        log::info!(
            "termux_bootstrap: rewrote com.termux paths in {} maintainer script(s)",
            rewritten
        );
    }
    Ok(())
}

fn rewrite_one_script(path: &Path) -> Result<bool> {
    const NEEDLE: &[u8] = b"/data/data/com.termux/";
    const REPLACEMENT: &[u8] = b"/data/data/dev.zed.zed_android/";
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
    // Single sed via shell glob over the four maintainer-script types.
    // `\\.` (single backslash + dot) is what apt's quoted-string parser
    // emits; the running shell sees `\.` and sed sees a literal `.`.
    // `2>/dev/null || true` keeps the hook silent and never fails apt
    // when the glob matches zero files (e.g. between unpack of a deb
    // with no scripts and the next configure pass).
    let body = format!(
        "// Auto-generated by gpui_android termux_bootstrap. Bridges the\n\
         // dpkg path-rewrite patches: rewrites com.termux references\n\
         // inside maintainer-script CONTENT (shebang + body) after every\n\
         // dpkg --unpack so that --configure can execve them.\n\
         DPkg::Post-Invoke {{\n    \
             \"sed -i 's|/data/data/com\\\\.termux/|/data/data/dev.zed.zed_android/|g' \
              {info}/*.preinst {info}/*.postinst {info}/*.prerm {info}/*.postrm \
              2>/dev/null || true\";\n\
         }};\n",
        info = info.display(),
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
/// `/data/data/com.termux/files/usr/lib` into every package's ELF
/// `DT_RUNPATH`. Without rewriting that, every freshly `pkg install`'d
/// binary fails at the dynamic-linker stage with
/// `library "libfoo.so" not found`. Layer 1 (our dpkg patches) puts
/// files at the right path; this hook makes those files actually
/// runnable.
///
/// Mechanism: `DPkg::Post-Invoke` apt hook fires after every dpkg
/// invocation. The hook calls a small shell helper that walks
/// `$PREFIX/{bin,sbin,libexec}` plus `$PREFIX/lib/*.so*`, filtered to
/// files modified in the last 10 minutes (so we touch only fresh
/// files, not the whole prefix on every dpkg run), and runs
/// `patchelf --set-rpath $PREFIX/lib --force-rpath` on each. patchelf
/// silently fails on non-ELF inputs and is idempotent for already-
/// correct RPATH, so the hook is safe to fire repeatedly.
///
/// Requires `patchelf` in the bootstrap (added via `--add patchelf`
/// to the Vultr `build-bootstraps.sh` invocation that produced r8+).
/// The helper script no-ops gracefully if patchelf is missing.
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
         # every dpkg --unpack to fix DT_RUNPATH on freshly-installed\n\
         # upstream Termux binaries. Termux's CI bakes\n\
         # /data/data/com.termux/files/usr/lib into every ELF; without\n\
         # this rewrite, the dynamic linker can't find shared libs and\n\
         # the binary fails to start.\n\
         #\n\
         # Critical correctness invariants (learned the hard way):\n\
         # 1. NO --force-rpath. It converts DT_RUNPATH→DT_RPATH and on\n\
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
         set -u\n\
         PREFIX={prefix_str}\n\
         WANT=\"$PREFIX/lib\"\n\
         [ -x \"$PREFIX/bin/patchelf\" ] || exit 0\n\
         maybe_patchelf() {{\n    \
             current=$(\"$PREFIX/bin/patchelf\" --print-rpath \"$1\" 2>/dev/null) || return 0\n    \
             [ \"$current\" = \"$WANT\" ] && return 0\n    \
             \"$PREFIX/bin/patchelf\" --set-rpath \"$WANT\" \"$1\" 2>/dev/null || true\n\
         }}\n\
         find \"$PREFIX/bin\" \"$PREFIX/sbin\" \"$PREFIX/libexec\" \
              -type f -cmin -1 2>/dev/null \
              | while IFS= read -r f; do maybe_patchelf \"$f\"; done\n\
         find \"$PREFIX/lib\" -type f -cmin -1 -name '*.so*' 2>/dev/null \
              | while IFS= read -r f; do maybe_patchelf \"$f\"; done\n\
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

/// Install `$PREFIX/bin/zed-setup-claude` — a one-shot helper that
/// turns a fresh `npm install -g @anthropic-ai/claude-code` into a
/// working `claude` command on Android. Handles all the layer-4/5 work
/// that npm can't (because npm's optional-deps system platform-skips
/// linux-arm64-musl on `process.platform === 'android'`).
///
/// What it does:
///   1. Force-install the linux-arm64-musl native binary package
///      (`npm install -g --force @anthropic-ai/claude-code-linux-arm64-musl`)
///   2. Patch claude-code's `install.cjs` to map `process.platform ===
///      'android'` to `'linux'` so its platform check accepts our
///      runtime
///   3. Run the patched `install.cjs` to hardlink the musl binary
///      into claude-code's `bin/claude.exe`
///   4. patchelf the binary's interpreter to our shipped musl linker
///      and RPATH to our prefix
///   5. Replace `$PREFIX/bin/claude` with a wrapper that strips
///      `LD_PRELOAD` (libtermux-exec is bionic, breaks musl) and
///      proot-binds our resolv.conf at `/etc/resolv.conf` (Bun's
///      musl-static DNS resolver hardcodes that path)
fn install_claude_setup_script(prefix: &Path) -> Result<()> {
    let bin_dir = prefix.join("bin");
    if !bin_dir.is_dir() {
        return Ok(());
    }
    let script_path = bin_dir.join("zed-setup-claude");
    let prefix_str_resolved = prefix.to_string_lossy();
    let prefix_str = prefix_str_resolved
        .strip_prefix("/data/user/0/")
        .map(|tail| format!("/data/data/{tail}"))
        .unwrap_or_else(|| prefix_str_resolved.to_string());
    let body = format!(
        "#!{prefix_str}/bin/bash\n\
         # Auto-generated by gpui_android termux_bootstrap.\n\
         # Turns a fresh `npm install -g @anthropic-ai/claude-code` into\n\
         # a runnable `claude` command on Android. Skips claude-code's\n\
         # install.cjs entirely — its optional-dep lookup can't find the\n\
         # musl variant (npm puts it at the global node_modules layer,\n\
         # install.cjs looks nested) and even if it found it, we'd need\n\
         # to patchelf afterwards anyway.\n\
         #\n\
         # Idempotent: safe to re-run after npm reinstalls / upgrades.\n\
         set -eu\n\
         PREFIX={prefix_str}\n\
         PKG_DIR=\"$PREFIX/lib/node_modules/@anthropic-ai/claude-code\"\n\
         MUSL_PKG_DIR=\"$PREFIX/lib/node_modules/@anthropic-ai/claude-code-linux-arm64-musl\"\n\
         MUSL_BIN=\"$MUSL_PKG_DIR/claude\"\n\
         CLAUDE_BIN=\"$PKG_DIR/bin/claude.exe\"\n\
         MUSL_LD=\"$PREFIX/lib/ld-musl-aarch64.so.1\"\n\
         \n\
         if [ ! -d \"$PKG_DIR\" ]; then\n    \
             # Try to install claude-code itself if missing — saves the\n    \
             # user from running 'npm install -g' separately.\n    \
             echo \"==> @anthropic-ai/claude-code not installed; installing\"\n    \
             if ! command -v npm >/dev/null 2>&1; then\n        \
                 echo \"error: npm not in PATH (pkg install nodejs)\" >&2\n        \
                 exit 1\n    \
             fi\n    \
             npm install -g @anthropic-ai/claude-code\n\
         fi\n\
         if [ ! -f \"$MUSL_LD\" ]; then\n    \
             echo \"error: musl linker missing at $MUSL_LD\" >&2\n    \
             echo \"the bootstrap should have installed this; reinstall the app\" >&2\n    \
             exit 1\n\
         fi\n\
         if ! command -v patchelf >/dev/null 2>&1; then\n    \
             echo \"error: patchelf not in PATH\" >&2\n    \
             exit 1\n\
         fi\n\
         \n\
         echo \"==> 1. force-installing @anthropic-ai/claude-code-linux-arm64-musl\"\n\
         npm install -g --force @anthropic-ai/claude-code-linux-arm64-musl >/dev/null 2>&1 || \\\n             npm install -g --force @anthropic-ai/claude-code-linux-arm64-musl\n\
         \n\
         if [ ! -f \"$MUSL_BIN\" ]; then\n    \
             echo \"error: $MUSL_BIN missing after install\" >&2\n    \
             exit 1\n\
         fi\n\
         \n\
         echo \"==> 2. copying musl binary into claude-code's bin/\"\n\
         mkdir -p \"$PKG_DIR/bin\"\n\
         cp -f \"$MUSL_BIN\" \"$CLAUDE_BIN\"\n\
         chmod +x \"$CLAUDE_BIN\"\n\
         \n\
         echo \"==> 3. patchelf interpreter + rpath on claude.exe\"\n\
         patchelf --set-interpreter \"$MUSL_LD\" \"$CLAUDE_BIN\"\n\
         patchelf --set-rpath \"$PREFIX/lib\" \"$CLAUDE_BIN\"\n\
         \n\
         echo \"==> 4. installing $PREFIX/bin/claude wrapper\"\n\
         rm -f \"$PREFIX/bin/claude\"\n\
         {{\n    \
             echo \"#!$PREFIX/bin/sh\"\n    \
             echo \"# Auto-generated by zed-setup-claude.\"\n    \
             echo \"# env -u LD_PRELOAD: libtermux-exec.so is bionic-linked,\"\n    \
             echo \"# loading it into musl claude.exe fails with symbol-not-found\"\n    \
             echo \"# errors and exits early.\"\n    \
             echo \"# proot bind: claude is a Bun-compiled musl-static binary\"\n    \
             echo \"# whose DNS resolver hardcodes /etc/resolv.conf, which doesn't\"\n    \
             echo \"# exist on Android. We surface our \\$PREFIX/etc/resolv.conf\"\n    \
             echo \"# there.\"\n    \
             echo \"exec env -u LD_PRELOAD $PREFIX/bin/proot -b \\\"$PREFIX/etc/resolv.conf:/etc/resolv.conf\\\" $CLAUDE_BIN \\\"\\$@\\\"\"\n         }} > \"$PREFIX/bin/claude\"\n\
         chmod +x \"$PREFIX/bin/claude\"\n\
         \n\
         echo\n\
         echo \"done. now run:  claude\"\n",
    );
    let needs_write = match std::fs::read(&script_path) {
        Ok(existing) => existing != body.as_bytes(),
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&script_path, body.as_bytes())
            .with_context(|| format!("write {}", script_path.display()))?;
        let mut perms = std::fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms)?;
        log::info!(
            "termux_bootstrap: wrote claude setup script at {}",
            script_path.display()
        );
    }
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
