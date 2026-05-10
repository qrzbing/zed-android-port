#![cfg(target_os = "android")]
//! Zed Workspace running on Android. Boots up the full client/project/
//! workspace stack and shows the WelcomePage on first launch (no auto-
//! opened project) — matches official Zed's first-run behaviour.

mod header;
mod menu_bar;
mod noexec_modal;
mod runtime_picker;
mod title_bar;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use android_activity::AndroidApp;
use anyhow::Result;
use client::{Client, UserStore};
use db::AppDatabase;
use db::kvp::KeyValueStore;
use fs::{Fs, RealFs};
use node_runtime::NodeRuntime;
use project::Project;
use session::{AppSession, Session};
use gpui::{App, AppContext as _, TaskExt as _, UpdateGlobal as _};
use log::{error, info};
use settings::{Settings as _, SettingsStore};
use util::ResultExt as _;
use workspace::{
    AppState, CloseIntent, CloseProject, MultiWorkspace, OpenOptions, Workspace, WorkspaceStore,
    open_new,
};
use reqwest_client::ReqwestClient;
use zdroid_runtime::{RuntimeId, config::RuntimeFile};

fn minimal_window_options(_: Option<uuid::Uuid>, _cx: &mut App) -> gpui::WindowOptions {
    gpui::WindowOptions::default()
}

/// Read the active runtime adapter from `runtime.toml`. Both
/// `android_main` (for env init) and `boot` (for terminal.shell) need
/// this; cheap enough to re-read in each rather than thread state.
/// Defaults to Bootstrap when toml is missing or unparseable so first-
/// launch UX matches today's behavior.
fn detect_runtime_id(data_path: &std::path::Path) -> RuntimeId {
    RuntimeFile::load(&data_path.join("usr/etc/zd-runtime.toml"))
        .ok()
        .flatten()
        .map(|f| f.runtime.kind)
        .unwrap_or(RuntimeId::Bootstrap)
}


// Bundled fonts. Upstream Zed walks `assets/fonts/` via `AssetSource`
// at boot (see `load_embedded_fonts` in crates/zed/src/main.rs) and
// hands every `.ttf` to `cx.text_system().add_fonts(...)`. The Android
// example doesn't run that loader, so until this list was filled out
// only Lilex-Regular existed in the text system: bold/italic fell back
// to faux-styled rasterizer output, IBM Plex Sans (the upstream UI
// font) was unavailable, and any `buffer_font_family` setting pointing
// at a different family silently no-op'd.
//
// `include_bytes!` baked into the .so means the fonts ship in the APK
// without going through AAssetManager — they're literally in the .so's
// rodata, so first read is mmap-direct with no extract / decompress
// step. Total budget across the 8 weights is ~3 MB, irrelevant against
// the bundled bootstrap zip.
const BUNDLED_FONTS: &[&[u8]] = &[
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Regular.ttf"),
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Bold.ttf"),
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Italic.ttf"),
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-BoldItalic.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf"),
];

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("zed_android"),
    );
    info!("zed_android: android_main entry");

    let data_path = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/data/com.zdroid/files"));
    info!("zed_android: data_path = {}", data_path.display());

    // SAFETY: set_var mutates libc-shared process state. The accurate
    // invariant is "no other thread is observed reading or writing libc env
    // via getenv/setenv at this point" — JVM service threads (GC,
    // finalizer, binder pool) exist by android_main but don't touch libc
    // env, so calling here is sound. Any later setenv from a callback
    // would be a soundness bug.
    //
    // OnceLock guard: android_main can re-enter on activity recreation. If
    // a Rust thread from the previous invocation outlives that boundary
    // and reads env via getenv/getenv_r, a second set_var would race. The
    // values are deterministic (same data_path), so the OnceLock makes
    // re-entry a no-op and keeps soundness independent of platform-side
    // thread teardown ordering.
    //
    // HOME stays pointed at data_path for compat with `dirs::home_dir()`
    // consumers in upstream zed (otherwise they panic on Android since the
    // sandbox has no system home). The Termux-style $TERMUX__HOME at
    // $ROOTFS/home is set separately so future shell/LSP children can
    // override HOME per-spawn without disturbing the Rust globals.
    //
    // PATH is unconditionally set to `$PREFIX/bin:$PATH` at boot. An
    // earlier version of this code gated the prepend on the presence of
    // `$PREFIX/bin/bash`, on the theory that pointing PATH at a
    // non-existent dir pre-bootstrap would break `which git` / `which
    // grep` lookups. That theory was wrong: PATH search ignores
    // non-existent entries and falls through to the next dir, so the
    // gate was pure downside — on a *fresh* install (data wiped /
    // first launch / app reinstalled), `extract_if_needed` runs AFTER
    // `ENV_INITIALIZED.get_or_init`, so bash isn't on disk yet, the
    // gate evaluates false, PATH stays Android-default, and the
    // OnceLock then blocks any later re-init. Result: integrated
    // terminal opens with PATH=`/system/bin:...` (no $PREFIX/bin), and
    // every `pkg`, `apt`, `dpkg` invocation hits "command not found"
    // until the user closes the terminal and re-opens it (which
    // reinherits the post-extract Zed env — but they have no idea
    // that's the workaround). Just always prepend; correctness is
    // unchanged post-extract, fresh-install UX no longer broken.
    let runtime_id = detect_runtime_id(&data_path);

    static ENV_INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = ENV_INITIALIZED.get_or_init(|| {
        let prefix = data_path.join("usr");
        let termux_home = data_path.join("home");
        unsafe {
            std::env::set_var("HOME", &data_path);
            std::env::set_var("TERMUX__ROOTFS", &data_path);
            std::env::set_var("PREFIX", &prefix);
            std::env::set_var("TERMUX__PREFIX", &prefix);
            std::env::set_var("TERMUX__HOME", &termux_home);
            // Read by our patched dpkg's tarfn.c at extract time. When set
            // and != "com.termux", dpkg rewrites tar entry paths starting
            // with /data/data/com.termux/ to /data/data/<this>/ on the fly.
            // Lets `pkg install <upstream-deb>` Just Work with our prefix.
            std::env::set_var("TERMUX_APP__PACKAGE_NAME", "com.zdroid");
            std::env::set_var("TMPDIR", prefix.join("tmp"));
            std::env::set_var("TERM", "xterm-256color");
            std::env::set_var("LANG", "en_US.UTF-8");
            std::env::set_var("COLORTERM", "truecolor");
            // Disable the remote-server source-build fallback in
            // `crates/remote/src/transport.rs::build_remote_server_from_source`
            // unconditionally on Android. The fallback fires when Zed can't
            // find a prebuilt remote_server binary for the target triple
            // (typical for any locally-built / non-release-channel client),
            // and tries to cross-compile the binary on the CLIENT — which
            // means rustup + zig + cargo-zigbuild + a glibc-target rustup
            // component on the device. None of that is reasonably available
            // inside Termux on bionic Android, and even if we shipped them
            // the cross-compile from aarch64-linux-android to
            // x86_64-unknown-linux-gnu (or whatever the remote runs) is a
            // multi-component lift. Setting `never` here forces Zed to use
            // the standard CDN-or-existing-on-remote path that production
            // clients use; users hitting a remote without a cached binary
            // either let Zed download from the CDN or pre-stage the binary
            // at `~/.cache/zed/remote_server` on the remote host themselves.
            std::env::set_var("ZED_BUILD_REMOTE_SERVER", "never");
            // Termux's bootstrap sets LD_PRELOAD=$PREFIX/lib/libtermux-exec.so
            // for its bash subprocesses (so exec() of Android-incompatible
            // shebangs gets shimmed). On Android this is fine — local
            // Termux subprocesses re-source `profile.d/termux-exec.sh` at
            // bash startup and re-set LD_PRELOAD themselves where they need
            // it. But our gpui app process inherits LD_PRELOAD from the
            // shell that launched it (or has it set explicitly elsewhere),
            // and when Zed's remote SSH subprocess inherits + propagates it
            // (either via OpenSSH's `SendEnv` or via the remote_server's
            // own env channel), the remote shell on every connection ends
            // up trying to dlopen our local Android-only path. ld.so on the
            // remote (typically glibc on Linux) prints `ld.so: object …
            // libtermux-exec.so from LD_PRELOAD cannot be preloaded:
            // ignored.` to stderr for every command. Non-fatal but turns
            // every remote-terminal session into a wall of spam. Clearing
            // it from the gpui app's env at boot keeps ssh subprocesses
            // clean while local Termux shells still get their shim via the
            // bash profile.
            std::env::remove_var("LD_PRELOAD");
            // (We deliberately do NOT flip ZED_RELEASE_CHANNEL to
            // "nightly" anymore. Channel switching namespaces Zed's
            // app data dir, which would shadow settings.json / recent
            // projects / ssh_connections set under whatever channel
            // the user already had data in. The Dev-channel hard-bail
            // at `crates/remote/src/transport/ssh.rs:850-855` is
            // patched directly — Dev-on-Android resolves
            // `wanted_version = None` same as Nightly, so the CDN
            // download path works regardless of channel. There's only
            // one ship target for our APK and the channel distinction
            // has no meaning here, so the upstream branch becomes a
            // static no-op rather than a runtime decision.)
            // Point HTTPS-using subprocesses (cargo, npm, curl, …) at
            // Termux's pre-shipped CA bundle. Without this, rust-
            // analyzer's `cargo metadata` dies with "unable to get
            // local issuer certificate" the first time cargo updates
            // the crates.io index, since cargo's curl has no fallback
            // location for a CA bundle on Android. SSL_CERT_FILE is
            // honored by openssl-rs + rustls + curl; CURL_CA_BUNDLE
            // covers older curl-built tooling that ignores the openssl
            // env var.
            let cert_path = prefix.join("etc/tls/cert.pem");
            if cert_path.is_file() {
                std::env::set_var("SSL_CERT_FILE", &cert_path);
                std::env::set_var("CURL_CA_BUNDLE", &cert_path);
            }
            // PATH + SHELL setup is now adapter-aware. The user's
            // selection in `runtime.toml` (written by the runtime
            // picker modal) decides whether sub-spawns route through
            // `$PREFIX/bin` directly (bootstrap mode, today's default
            // and what the editor inherits if no toml exists) or
            // through `$PREFIX/zd-runtime/<name> -> zd-exec` symlinks
            // that hand off to `zd-spawnd` for chroot dispatch.
            //
            // The chroot path is now safe to prepend: `zd-runtime/<name>`
            // re-exec's the Rust `zd-exec` wrapper (not a bash script
            // that re-shells through `su`), and the wrapper talks
            // directly to the persistent `zd-spawnd` daemon over a Unix
            // socket. ~5ms per spawn, no Magisk su mediation, no
            // fork-bomb risk under Zed's startup load.
            //
            // External-Termux mode falls through to bootstrap-style env
            // until the JNI Intent bridge lands (task #36). At that
            // point spawning will go through Java not exec, so PATH
            // doesn't gate it anyway.
            log::info!("zed_android: runtime adapter = {:?}", runtime_id);

            let zed_bin = prefix.join(".zed/bin");
            let prefix_bin = prefix.join("bin");
            let zd_runtime = prefix.join("zd-runtime");
            let existing = std::env::var_os("PATH").unwrap_or_default();
            let mut new_path = std::ffi::OsString::new();
            if matches!(runtime_id, RuntimeId::Chroot) {
                new_path.push(&zd_runtime);
                new_path.push(":");
            }
            new_path.push(&zed_bin);
            new_path.push(":");
            new_path.push(&prefix_bin);
            new_path.push(":");
            new_path.push(&existing);
            std::env::set_var("PATH", &new_path);

            // SHELL points at the wrapper for chroot mode so the
            // integrated terminal lands inside the rootfs. Bootstrap
            // mode keeps today's bionic bash — same UX as before.
            let shell = match runtime_id {
                RuntimeId::Chroot => prefix.join("bin/zd-exec"),
                RuntimeId::Bootstrap | RuntimeId::ExternalTermux => prefix.join("bin/bash"),
            };
            std::env::set_var("SHELL", &shell);

            // DELIBERATELY NOT SET: LD_LIBRARY_PATH.
            //
            // Our bootstrap binaries (built with TERMUX_APP_PACKAGE=
            // com.zdroid) have DT_RUNPATH pointing at our real lib
            // path, so they load libs natively without help.
            //
            // Setting LD_LIBRARY_PATH globally poisons every spawned
            // subprocess, including Android system processes — e.g.
            // /system/bin/app_process64 loads /system/lib64/libsqlite.so
            // which needs OpenSSL_add_all_algorithms; the linker
            // searches LD_LIBRARY_PATH first, finds our OpenSSL 3.x
            // libssl.so (which dropped that deprecated symbol),
            // CANNOT LINK, cascading into JVM stack overflow when ART
            // retries the failing dlopen. Crashed copy/paste from the
            // terminal panel because of exactly this.
            //
            // Upstream Termux packages we install via `pkg` DO need
            // LD_LIBRARY_PATH to find their libs at runtime, but we
            // set it per-spawn (in the LSP launcher / terminal-panel
            // pty bringup), not as a global env var.
            log::info!(
                "zed_android: PATH = {}; SHELL = {}",
                new_path.to_string_lossy(),
                shell.display(),
            );
        }
    });

    // Surface the SELinux domain in logcat — this is the canary for the
    // targetSdk pin. If `untrusted_app_27` flips to `untrusted_app_all`
    // every subsequent execve into $PREFIX/bin/* will EACCES.
    gpui_android::termux_bootstrap::check_selinux_context();

    // Best-effort runtime READ/WRITE_EXTERNAL_STORAGE prompt. Replaces the
    // MANAGE_EXTERNAL_STORAGE → Settings deep-link flow we used at
    // targetSdk=35. Fire-and-forget: dialog shows on first launch, user
    // grants once, RealFs reads of /storage/emulated/0/... start working.
    gpui_android::storage::request_once(&app);

    // Materialize Android's active DNS servers into /sdcard/.zed/r so
    // hex-patched Bun-compiled CLIs (claude, codex, future) can resolve
    // hostnames without proot. Patched binaries open `/sdcard/.zed/r`
    // instead of the original `/etc/resolv.conf`. Falls back to public
    // DNS if ConnectivityManager gives nothing (no active network yet).
    gpui_android::dns_bridge::populate_resolv_conf(&app);

    // Best-effort first-launch extraction of the bundled Termux bootstrap.
    // Non-fatal: if the asset isn't bundled yet (pre-L2a/L2b) the extractor
    // logs and returns Err; the editor still boots without a runtime, the
    // integrated terminal and pkg-installed LSPs just stay unavailable.
    if let Err(err) =
        gpui_android::termux_bootstrap::extract_if_needed(&app, &data_path)
    {
        log::warn!(
            "zed_android: termux bootstrap extract failed: {err:#}; \
             continuing without integrated runtime"
        );
    }
    // Idempotent runtime fixes — rewrite com.termux strings inside
    // maintainer-script bodies, install the apt Post-Invoke hook so
    // future `pkg install` triggers the same rewrite. Runs on every
    // boot regardless of whether extraction actually re-extracted; the
    // sed is no-op on clean files and the apt config write is constant.
    if let Err(err) =
        gpui_android::termux_bootstrap::apply_runtime_patches(&app, &data_path)
    {
        log::warn!(
            "zed_android: termux runtime patches failed: {err:#}; \
             upstream `pkg install` of packages with hardcoded shebangs \
             may need a manual sed + dpkg --configure -a"
        );
    }

    // Wire askpass to the standalone helper now that
    // apply_runtime_patches has placed it at $PREFIX/bin/zed-askpass-helper.
    // Must happen BEFORE any AskPassSession is created (Open Remote,
    // git auth prompts, etc.) — the askpass crate's ASKPASS_PROGRAM
    // OnceLock initializes on first read with current_exe() (=
    // /system/bin/app_process64 on Android) and subsequent set_program
    // calls are silently ignored.
    let askpass_path = data_path.join("usr/bin/zed-askpass-helper");
    if askpass_path.is_file() {
        match askpass::set_program(askpass_path.clone()) {
            Ok(()) => log::info!(
                "zed_android: askpass program set to {}",
                askpass_path.display()
            ),
            Err(_) => log::warn!(
                "zed_android: askpass::set_program rejected (OnceLock \
                 already initialized — set_program must run BEFORE first \
                 AskPassSession)"
            ),
        }
    } else {
        log::warn!(
            "zed_android: askpass helper missing at {}; SSH password / \
             passphrase prompts will fall through to current_exe() and \
             abort under SELinux",
            askpass_path.display()
        );
    }

    gpui_android::run(app, assets::Assets, move |cx: &mut App| {
        if let Err(err) = boot(cx, &data_path) {
            error!("zed_android: boot failed: {err:#}");
        }
    });
}

fn boot(cx: &mut App, data_path: &std::path::Path) -> Result<()> {
    // android_main can run multiple times for one process when the activity
    // is recreated; paths' OnceLocks survive across invocations. The second
    // call panics with "set_custom_data_dir called after data_dir or
    // config_dir was initialized". Guard with a static flag.
    static PATHS_INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = PATHS_INITIALIZED.get_or_init(|| {
        paths::set_custom_data_dir(&data_path.to_string_lossy());
    });
    info!("zed_android: paths data_dir set");

    // Production zed creates these in `init_paths()` at boot. Without
    // them, anything using `update_settings_file` (Onboarding theme /
    // keymap toggles, settings_ui writes, workspace serialization)
    // tries to atomic_write into a directory that doesn't exist, fails,
    // and short-circuits before even applying the in-memory mutation —
    // so toggles look like no-ops.
    let termux_home = data_path.join("home");
    let projects_dir = termux_home.join("projects");
    for path in [
        paths::config_dir(),
        paths::database_dir(),
        paths::logs_dir(),
        paths::temp_dir(),
        paths::languages_dir(),
        &termux_home,
        // Default landing dir for the project picker. Lives on app-private
        // storage where execve and dlopen work natively, so cargo / go /
        // make / native-npm / native-pip all just run. SAF-picked projects
        // get imported here; nothing executable ever runs from /sdcard.
        &projects_dir,
    ] {
        if let Err(err) = std::fs::create_dir_all(path) {
            error!("zed_android: create_dir_all({}): {err:#}", path.display());
        }
    }
    // Termux-style ~/storage/* curated symlinks into shared storage. Lets
    // users browse / open / save individual files from /sdcard via
    // ~/storage/{shared,downloads,dcim,...} without ever treating those
    // paths as a workspace root (which would hit the FUSE noexec wall on
    // any compile-and-run flow). Idempotent on re-launch.
    gpui_android::storage::setup_user_symlinks(&termux_home);

    // Pre-trust every repo. Files under /storage/emulated/0 are owned by
    // media_rw (UID 1023) but we run as the app's per-app UID, so libgit2's
    // dubious-ownership check fires for every repo a user opens. The
    // `Trust Directory` button in git_panel writes `safe.directory = <path>`
    // to ~/.gitconfig per-repo; this skips that prompt globally by setting
    // `safe.directory = *`. Idempotent: only writes if the file doesn't
    // already exist (we never want to clobber a user's git config).
    let gitconfig = data_path.join(".gitconfig");
    if !gitconfig.exists() {
        let body = "[safe]\n\tdirectory = *\n";
        match std::fs::write(&gitconfig, body) {
            Ok(()) => info!("zed_android: wrote default ~/.gitconfig with safe.directory = *"),
            Err(err) => error!(
                "zed_android: failed to write ~/.gitconfig at {}: {err:#}",
                gitconfig.display()
            ),
        }
    }

    release_channel::init(semver::Version::new(0, 1, 0), cx);
    info!("zed_android: release_channel init");

    info!("zed_android: http client init");
    let http = ReqwestClient::user_agent("zed_android/0.1")?;
    cx.set_http_client(Arc::new(http));
    info!("zed_android: http client set");

    info!("zed_android: settings + theme + editor init");
    settings::init(cx);
    // `gpui_tokio::init` registers the global Tokio runtime that wasmtime's
    // async support taps into for extension epoch interruption + WASI
    // async tasks. Production calls this at zed/src/main.rs:462. Without
    // it, extension_host's first WASM extension load panics:
    //   `RustPanic: no state of type gpui_tokio::GlobalTokio exists`
    // (caught it once on Android, then added this).
    gpui_tokio::init(cx);
    // `extension::init` creates the global ExtensionHostProxy that all
    // extension contribution registries (theme, language, debug-adapter)
    // hang off of. Has to run BEFORE any of those `*_extension::init`
    // calls. Cheap — just installs a default proxy global; the real
    // store gets created later by `extension_host::init` once fs/client
    // /node_runtime are available.
    extension::init(cx);
    theme_settings::init(theme::LoadThemes::All(Box::new(assets::Assets)), cx);
    editor::init(cx);

    let app_db = AppDatabase::new();
    cx.set_global(app_db);
    info!("zed_android: AppDatabase opened + set as global");

    let session_id = uuid::Uuid::new_v4().to_string();
    let session = gpui::block_on(Session::new(session_id, KeyValueStore::global(cx)));
    info!("zed_android: Session opened");

    let client = Client::production(cx);
    info!("zed_android: Client::production constructed (id={})", client.id());

    // Initialize auto_update so the remote_server CDN-fetch path can
    // resolve the right binary URL via `GlobalAutoUpdate`. The
    // remote/transport.rs flow at `auto_update.rs:516` reads this
    // global to know which `zed-remote-server` asset to fetch from
    // GitHub releases; without it, every Open Remote attempt hard-bails
    // with "auto-update not initialized" before any download starts.
    //
    // We deliberately set `ZED_UPDATE_EXPLANATION` ahead of `init` so
    // the polling subscription (auto_update.rs:248-262) is suppressed —
    // we DO NOT want Zed periodically self-updating the APK at runtime
    // (Android distribution = user reinstalls the APK, not in-app
    // update). The explanation string is shown if the user manually
    // hits "Check for updates", redirecting them to reinstall.
    unsafe {
        std::env::set_var(
            "ZED_UPDATE_EXPLANATION",
            "Updates ship via the APK; reinstall to upgrade.",
        );
    }
    auto_update::init(client.clone(), cx);
    info!("zed_android: auto_update::init complete (polling suppressed)");

    let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
    let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));
    info!("zed_android: UserStore + WorkspaceStore constructed");

    let fs: Arc<dyn Fs> = Arc::new(RealFs::new(None, cx.background_executor().clone()));
    <dyn Fs>::set_global(fs.clone(), cx);
    // Real NodeRuntime, mirroring crates/zed/src/main.rs:496-518. The
    // earlier port stage stubbed this out as `NodeRuntime::unavailable()`
    // back when Termux's Node didn't run on bionic — pre L3 npm intercept
    // architecture (project_l3_npm_intercept memory). The intercept layer
    // (patched node platform string, npm wrapper, launcher-gen RUNPATH
    // fixup, libtermux-exec LD_PRELOAD) plus the bundled musl loader make
    // a Termux-installed Node usable. Wire NodeRuntime against settings
    // so PATH-resolved Node (`pkg install nodejs` → $PREFIX/bin/node)
    // works, and Zed's managed-node fallback download path is available
    // for users without termux Node. Without this, npm-based LSPs
    // (TypeScript / JavaScript / Pyright / etc.) fail at the install step
    // with `'node' settings do not allow any way to use Node.js`.
    let (mut node_options_tx, node_options_rx) =
        watch::channel::<Option<node_runtime::NodeBinaryOptions>>(None);
    cx.observe_global::<SettingsStore>(move |cx| {
        let settings = &project::project_settings::ProjectSettings::get_global(cx).node;
        let options = node_runtime::NodeBinaryOptions {
            allow_path_lookup: !settings.ignore_system_version,
            allow_binary_download: true,
            use_paths: settings.path.as_ref().map(|node_path| {
                let node_path = std::path::PathBuf::from(
                    shellexpand::tilde(node_path).as_ref(),
                );
                let npm_path = settings.npm_path.as_ref().map(|p| {
                    std::path::PathBuf::from(shellexpand::tilde(&p).as_ref())
                });
                (
                    node_path.clone(),
                    npm_path.unwrap_or_else(|| {
                        node_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_default()
                            .join("npm")
                    }),
                )
            }),
        };
        node_options_tx.send(Some(options)).log_err();
    })
    .detach();
    let node_runtime = NodeRuntime::new(client.http_client(), None, node_options_rx);
    info!("zed_android: RealFs + NodeRuntime constructed (PATH lookup + managed download)");

    // Mirror production zed::watch_settings_files. Without this, edits to
    // ~/.config/zed/settings.json on disk never propagate into the running
    // app — themes, keybindings, terminal.shell etc. only honoured on
    // restart. Production also wires migration notifications; we skip those
    // because we don't ship the upgrade path UI yet.
    settings::SettingsStore::update_global(cx, |store, cx| {
        store.watch_settings_files(fs.clone(), cx, |settings_file, result, _cx| {
            if let settings::ParseStatus::Failed { error } = &result.parse_status {
                log::error!(
                    "zed_android: settings parse failed ({settings_file:?}): {error}"
                );
            }
        });
    });
    info!("zed_android: settings file watcher attached");

    cx.text_system().add_fonts(
        BUNDLED_FONTS
            .iter()
            .map(|bytes| Cow::Borrowed(*bytes))
            .collect(),
    )?;

    let mut language_registry =
        language::LanguageRegistry::new(cx.background_executor().clone());
    language_registry.set_language_server_download_dir(paths::languages_dir().clone());
    let language_registry = Arc::new(language_registry);

    languages::init(
        language_registry.clone(),
        fs.clone(),
        node_runtime.clone(),
        cx,
    );
    info!("zed_android: languages::init complete (load-grammars feature on)");

    // language_extension::init registers grammar/language/language-server
    // proxies on the global ExtensionHostProxy so extension-installed
    // languages and LSPs route through Zed's normal language registry.
    // Must come AFTER `languages::init` (the registry has to exist) and
    // AFTER `extension::init` (the proxy has to exist).
    //
    // Production passes `LspAccess::ViaWorkspaces(...)` so extension-
    // installed LSPs get auto-registered against every active workspace.
    // We use `Noop` for now — extensions can still contribute languages,
    // grammars, and themes; LSP-from-extension will require wiring
    // `ViaWorkspaces` against the multi-workspace scope, which is its
    // own follow-up because Android's `MultiWorkspace` differs in shape
    // from desktop.
    {
        let extension_host_proxy = extension::ExtensionHostProxy::global(cx);
        language_extension::init(
            language_extension::LspAccess::Noop,
            extension_host_proxy,
            language_registry.clone(),
        );
    }

    let registry = theme::ThemeRegistry::global(cx);
    info!(
        "zed_android: theme registry has {} themes loaded",
        registry.list().len()
    );

    let app_session = cx.new(|cx| AppSession::new(session, cx));
    let app_state = Arc::new(AppState {
        languages: language_registry,
        client: client.clone(),
        user_store,
        workspace_store,
        fs: fs.clone(),
        build_window_options: minimal_window_options,
        node_runtime: node_runtime.clone(),
        session: app_session,
    });
    AppState::set_global(app_state.clone(), cx);
    info!("zed_android: AppState assembled + set as global");

    // Mirror production zed/src/main.rs:785 — without this every language's
    // tree-sitter captures parse but render with no syntax styling. Re-apply
    // on theme changes so theme toggles actually recolour text. Bracket
    // matching, indents, and outline don't depend on this and would have
    // worked already; syntax colors and rainbow brackets do.
    app_state.languages.set_theme(theme::GlobalTheme::theme(cx).clone());
    cx.observe_global::<theme::GlobalTheme>({
        let languages = app_state.languages.clone();
        move |cx| {
            languages.set_theme(theme::GlobalTheme::theme(cx).clone());
        }
    })
    .detach();

    // Adapter-aware `terminal.shell`. The picker writes runtime.toml;
    // we re-derive the spawn target every launch so flipping the
    // adapter actually changes where the integrated terminal lands.
    //
    // Why overwrite even when the user already has a shell set: the
    // picker IS the user's terminal-target choice in this app — picking
    // chroot means "I want the terminal in the chroot's bash". Honoring
    // a stale settings.shell from a prior adapter would silently
    // contradict the picker. A user who wants something custom inside
    // the chroot configures it inside the chroot (their `$SHELL`,
    // `chsh`, etc.), not at the alacritty entry point.
    //
    // Two adapter shells:
    //   - chroot:  $PREFIX/bin/zd-exec (Rust wrapper → zd-spawnd → kali)
    //   - else:    $PREFIX/bin/bash    (today's bionic Termux bash)
    //
    // alacritty's `pw_shell` lookup on Android returns /system/bin/sh
    // (the parody) so this explicit Shell::Program is what makes any
    // useful terminal work at all.
    let runtime_id = detect_runtime_id(data_path);
    let shell_path = match runtime_id {
        RuntimeId::Chroot => data_path.join("usr/bin/zd-exec"),
        RuntimeId::Bootstrap | RuntimeId::ExternalTermux => data_path.join("usr/bin/bash"),
    };
    if shell_path.is_file() {
        let shell_str = shell_path.to_string_lossy().to_string();
        let fs_for_settings = app_state.fs.clone();
        cx.global::<settings::SettingsStore>().update_settings_file(
            fs_for_settings,
            move |content, _cx| {
                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                let new_shell = settings::Shell::Program(shell_str.clone());
                let prev = terminal.project.shell.clone();
                terminal.project.shell = Some(new_shell);
                log::info!(
                    "zed_android: terminal.shell -> {} (was {:?}, adapter-derived)",
                    shell_str,
                    prev,
                );
            },
        );
    } else {
        log::warn!(
            "zed_android: adapter-derived shell {} missing on disk; \
             leaving terminal.shell as-is",
            shell_path.display()
        );
    }

    Client::set_global(client.clone(), cx);
    client::init(&client, cx);
    // Register the TrustedWorktrees global. git_store reads it via
    // `try_get_global` when a repo is added; without it, `is_trusted`
    // defaults to false, which routes every repo into the
    // dubious-ownership UI in git_panel.rs:4862.
    //
    // Mirror production zed/src/main.rs:450-457 — fetch the persisted
    // trust grants from `WorkspaceDb::fetch_trusted_worktrees()` so a
    // user's prior "Trust" choice survives across launches and
    // reinstalls. Previously we passed `HashMap::default()` here, which
    // wiped the in-memory trust map every boot even though the SQLite
    // db was preserving it correctly — every relaunch re-prompted the
    // restricted-mode trust dialog. Fall back to empty on fetch failure
    // (typically only happens if the db schema upgrade is mid-flight).
    let db_trusted_paths =
        match workspace::WorkspaceDb::global(cx).fetch_trusted_worktrees() {
            Ok(paths) => paths,
            Err(err) => {
                error!(
                    "zed_android: fetch_trusted_worktrees failed at boot: \
                     {err:#} — starting with empty trust map; user will \
                     be re-prompted for any previously trusted projects"
                );
                std::collections::HashMap::default()
            }
        };
    project::trusted_worktrees::init(db_trusted_paths, cx);
    Project::init(&client, cx);

    // Extensions (Phase L3 — previously deferred). Mirrors production
    // zed/src/main.rs:623, 631–637, 741. The store creates the
    // global ExtensionStore, watches `paths::extensions_dir()` for new
    // installations, and asynchronously fetches the registry from
    // Zed's API. Theme/debug-adapter proxies register against the
    // already-installed `ExtensionHostProxy` from `extension::init`
    // earlier in boot. extensions_ui registers the workspace observer
    // that handles the `zed_actions::Extensions` action — taps from
    // the title-bar settings menu open the browse/install pane.
    //
    // Wasmtime engine init (`crates/extension_host/src/wasm_host.rs:564`)
    // currently uses Cranelift JIT. Whether that survives Android's
    // `untrusted_app_27` SELinux W^X policy is open — modern Android
    // (API 30+) typically allows anonymous executable mappings for
    // app processes, so it MAY just work; if not, the engine init
    // will panic at first extension load and we switch wasmtime's
    // strategy to Pulley interpreter (one Cargo.toml feature flip +
    // a `Config::strategy(Strategy::Pulley)` line). See
    // `deferred-render-pipeline-perf.md` philosophy: read the actual
    // failure first, don't preemptively configure for a problem that
    // may not exist.
    {
        let extension_host_proxy = extension::ExtensionHostProxy::global(cx);
        extension_host::init(
            extension_host_proxy.clone(),
            fs.clone(),
            client.clone(),
            node_runtime.clone(),
            cx,
        );
        debug_adapter_extension::init(extension_host_proxy.clone(), cx);
        theme_extension::init(
            extension_host_proxy,
            theme::ThemeRegistry::global(cx),
            cx.background_executor().clone(),
        );
    }

    diagnostics::init(cx);
    workspace::init(app_state.clone(), cx);
    command_palette::init(cx);
    search::init(cx);
    // Mirror production zed/src/main.rs:710. setup_search_bar is fired
    // from `terminal_panel.rs::TerminalPanel::new` ONLY — i.e. when the
    // terminal panel adds its own internal buffer search. It is NOT
    // called for editor panes; production wires those via initialize_pane
    // in zed/src/zed.rs:1234, which we mirror further down inside the
    // workspace observe_new (so editor-pane toolbars get BufferSearchBar
    // + ProjectSearchBar there).
    cx.set_global(workspace::PaneSearchBarCallbacks {
        setup_search_bar: |languages, toolbar, window, cx| {
            let search_bar = cx.new(|cx| search::BufferSearchBar::new(languages, window, cx));
            toolbar.update(cx, |toolbar, cx| {
                toolbar.add_item(search_bar, window, cx);
            });
        },
        wrap_div_with_search_actions: search::buffer_search::register_pane_search_actions,
    });
    vim::init(cx);
    // Modal pickers / panels — mirror production zed/src/main.rs init order.
    // Each registers its own actions + a SettingsStore observer if needed.
    // Skipped from production (non-portable on Android): audio/call/livekit
    // (collab), agent_ui/copilot/language_models (AI), debugger_ui/repl
    // (DAP/Jupyter), auto_update (we self-distribute), telemetry/crashes,
    // extension_host (deferred to L3).
    go_to_line::init(cx);
    file_finder::init(cx);
    tab_switcher::init(cx);
    outline::init(cx);
    project_symbols::init(cx);
    project_panel::init(cx);
    outline_panel::init(cx);
    tasks_ui::init(cx);
    snippet_provider::init(cx);
    snippets_ui::init(cx);
    image_viewer::init(cx);
    csv_preview::init(cx);
    svg_preview::init(cx);
    markdown_preview::init(cx);
    encoding_selector::init(cx);
    language_selector::init(cx);
    line_ending_selector::init(cx);
    toolchain_selector::init(cx);
    theme_selector::init(cx);
    settings_profile_selector::init(cx);
    language_tools::init(cx);
    feedback::init(cx);
    // language_model::init only registers the GlobalLanguageModelRegistry —
    // no actual AI runtime spins up. git_panel.rs reads from this registry
    // for the optional commit-message generation; without it, the panel
    // panics at first paint with "no state of type
    // language_model::registry::GlobalLanguageModelRegistry exists".
    language_model::init(cx);
    git_ui::init(cx);
    // Mirror production zed/src/main.rs:733 — register the git graph
    // (commit history) view's serializable item, action handlers
    // (git::FileHistory, git_panel::Open, OpenAtCommit), and database
    // domain. Action-driven: shows up as a workspace pane item when the
    // user triggers it (e.g. via git panel "View History"), not pre-
    // loaded as a panel like ProjectPanel/GitPanel. Uses
    // project.git_store() so remote-SSH projects work transparently.
    git_graph::init(cx);
    // Production zed/src/main.rs:741. Registers the workspace observer
    // that handles `zed_actions::Extensions::default()` — opens the
    // browse/install/manage pane (an `ExtensionsPage` workspace item).
    // The settings-menu chevron in the Android title bar already
    // dispatches `zed_actions::Extensions` (see title_bar.rs); without
    // this init the action goes nowhere.
    extensions_ui::init(cx);
    keymap_editor::init(cx);
    inspector_ui::init(app_state.clone(), cx);
    json_schema_store::init(cx);
    recent_projects::init(cx);
    which_key::init(cx);
    settings_ui::init(cx);
    workspace::init_settings_file_actions(cx);
    editor::init_bundled_file_actions(cx);
    terminal_view::init(cx);
    // Onboarding reads `AllAgentServersSettings` from the SettingsStore;
    // `SettingsStore::get` panics if the type isn't registered, so register
    // it before any onboarding render. agent_settings doesn't expose a
    // separate init function — registering the type is the whole step.
    project::agent_server_store::AllAgentServersSettings::register(cx);
    onboarding::init(cx);
    menu_bar::register_actions(cx);
    runtime_picker::register(cx);
    info!(
        "zed_android: workspace + diagnostics + search + file_finder + outline_panel + onboarding + menu_bar init complete"
    );

    // Keymap load runs LAST: `_allow_partial_failure` skips any binding
    // whose action isn't registered yet, so loading before each crate's
    // `init` would silently drop every workspace::*, project_panel::*,
    // command_palette::*, vim::*, etc. binding. By the time we reach this
    // point everything has registered.
    match settings::KeymapFile::load_asset_allow_partial_failure(
        settings::DEFAULT_KEYMAP_PATH,
        cx,
    ) {
        Ok(bindings) => {
            info!("zed_android: loaded {} key bindings from default keymap", bindings.len());
            cx.bind_keys(bindings);
        }
        Err(err) => error!("zed_android: keymap load failed: {err:#}"),
    }

    // Mirror production's zed/src/zed.rs `initialize_panels`: every
    // newly-constructed workspace asynchronously loads each panel and
    // attaches it. Production loads project / outline / terminal / git /
    // collab / debug / agent panels in parallel; we only ship the ones
    // whose deps are already wired (project_panel, outline_panel for now).
    // The PanelButtons in the status bar render one button per
    // registered panel, so adding more panels here surfaces more
    // bottom-bar buttons for free.
    let observe_app_state = app_state.clone();
    cx.observe_new(move |workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };

        workspace.register_action(editor::open_project_settings_file);

        // CloseProject: action wired in production at
        // `crates/zed/src/zed.rs:1123-1180`. Production binds it inside
        // `initialize_workspace` next to NewFile / NewWindow handlers.
        // We don't pull crates/zed (Android workspace example owns its
        // own boot()), so the handler has to be re-registered here on
        // every new workspace via observe_new.
        //
        // Flow, identical to production except the new-workspace init
        // closure is a no-op (we don't auto-create an empty Editor on
        // close — same shape as the returning-launch path at
        // `workspace::open_new(Default::default(), app_state, cx,
        // |_, _, _| {})` lower in this file):
        //
        //   1. prepare_to_close(ReplaceWindow) — checks dirty buffers,
        //      pops the standard "Save changes?" modal if needed.
        //   2. If the user proceeds, open_new() builds a fresh empty
        //      workspace within the same MultiWorkspace tab, reusing
        //      the requesting_window so we don't spawn an extra
        //      ExtraWindowActivity (which open_window does on Android).
        //   3. After the new workspace lands, remove the old project's
        //      group key from the MultiWorkspace registry. Without this
        //      the closed project's group survives in MultiWorkspace's
        //      internal tracking and `Open Recent` / window-cycle UX
        //      shows ghost entries.
        workspace.register_action({
            let app_state = observe_app_state.clone();
            move |workspace: &mut Workspace, _: &CloseProject, window, cx| {
                let Some(window_handle) =
                    window.window_handle().downcast::<MultiWorkspace>()
                else {
                    return;
                };
                let app_state = app_state.clone();
                let old_group_key = workspace.project_group_key(cx);
                cx.spawn_in(window, async move |this, cx| {
                    let should_continue = this
                        .update_in(cx, |workspace, window, cx| {
                            workspace.prepare_to_close(
                                CloseIntent::ReplaceWindow,
                                window,
                                cx,
                            )
                        })?
                        .await?;
                    if !should_continue {
                        return Ok::<(), anyhow::Error>(());
                    }
                    let task = cx.update(|_window, cx| {
                        open_new(
                            OpenOptions {
                                requesting_window: Some(window_handle),
                                ..Default::default()
                            },
                            app_state,
                            cx,
                            |_workspace, _window, _cx| {},
                        )
                    })?;
                    task.await?;
                    window_handle
                        .update(cx, |mw, window, cx| {
                            mw.remove_project_group(&old_group_key, window, cx)
                        })?
                        .await
                        .log_err();
                    Ok(())
                })
                .detach_and_log_err(cx);
            }
        });

        // Status bar items, mirroring production zed/src/zed.rs:537-586.
        // Skipped: edit_prediction_ui (AI), activity_indicator (collab),
        // merge_conflict_indicator (git_ui — pulls collab), image_info
        // (image-viewer specific). The rest are plumbed identically.
        let search_button = cx.new(|_| search::search_status_button::SearchButton::new());
        let diagnostic_summary =
            cx.new(|cx| diagnostics::items::DiagnosticIndicator::new(workspace, cx));
        let active_file_name = cx.new(|_| workspace::active_file_name::ActiveFileName::new());
        let active_buffer_encoding =
            cx.new(|_| encoding_selector::ActiveBufferEncoding::new(workspace));
        let active_buffer_language =
            cx.new(|_| language_selector::ActiveBufferLanguage::new(workspace));
        let active_toolchain_language =
            cx.new(|cx| toolchain_selector::ActiveToolchain::new(workspace, window, cx));
        let vim_mode_indicator = cx.new(|cx| vim::ModeIndicator::new(window, cx));
        let cursor_position =
            cx.new(|_| go_to_line::cursor_position::CursorPosition::new(workspace));
        let line_ending_indicator =
            cx.new(|_| line_ending_selector::LineEndingIndicator::default());
        let merge_conflict_indicator =
            cx.new(|cx| git_ui::MergeConflictIndicator::new(workspace, cx));

        // LspButton needs a handle for the toggle action, same as production.
        let lsp_button_menu_handle =
            ui::PopoverMenuHandle::<ui::ContextMenu>::default();
        let lsp_button = cx.new(|cx| {
            language_tools::lsp_button::LspButton::new(
                workspace,
                lsp_button_menu_handle.clone(),
                window,
                cx,
            )
        });
        workspace.register_action({
            let handle = lsp_button_menu_handle.clone();
            move |_, _: &language_tools::lsp_button::ToggleMenu, window, cx| {
                handle.toggle(window, cx);
            }
        });

        workspace.status_bar().update(cx, |status_bar, cx| {
            status_bar.add_left_item(search_button, window, cx);
            status_bar.add_left_item(lsp_button, window, cx);
            status_bar.add_left_item(diagnostic_summary, window, cx);
            status_bar.add_left_item(active_file_name, window, cx);
            status_bar.add_left_item(merge_conflict_indicator, window, cx);
            status_bar.add_right_item(active_buffer_encoding, window, cx);
            status_bar.add_right_item(active_buffer_language, window, cx);
            status_bar.add_right_item(active_toolchain_language, window, cx);
            status_bar.add_right_item(line_ending_indicator, window, cx);
            status_bar.add_right_item(vim_mode_indicator, window, cx);
            status_bar.add_right_item(cursor_position, window, cx);
        });

        // Per-pane toolbar items. Mirrors production
        // zed/src/zed.rs::initialize_pane (zed.rs:1234) which production
        // invokes from a workspace observer at zed.rs:444-451 for the
        // initial active pane and on every PaneAdded event.
        //
        //   - BufferSearchBar — the in-editor Ctrl-F find/replace bar.
        //     Already registered for terminal panels via the global
        //     PaneSearchBarCallbacks above; production registers a SECOND
        //     instance on each editor pane's toolbar (different toolbar,
        //     different visibility scope), and we mirror that here.
        //
        //   - ProjectSearchBar — the toolbar that holds the actual query
        //     input field, regex/case/whole-word toggles, replace UI,
        //     and match navigation for the Project Search view.
        //     ProjectSearchView's render() draws only the results body;
        //     the input field lives in this toolbar item. Without it
        //     registered on the pane's toolbar, the project-search view
        //     shows the empty-state heading but no input field, and the
        //     toolbar slot stays in a half-rendered state — visible as
        //     a flickering tab at vsync rate.
        //
        // Subscribe to PaneAdded so split panes get the same setup.
        let toolbar_languages = observe_app_state.languages.clone();
        let active_pane = workspace.active_pane().clone();
        active_pane.update(cx, |pane, cx| {
            pane.toolbar().update(cx, |toolbar, cx| {
                let buffer_search_bar = cx.new(|cx| {
                    search::BufferSearchBar::new(
                        Some(toolbar_languages.clone()),
                        window,
                        cx,
                    )
                });
                toolbar.add_item(buffer_search_bar, window, cx);
                let project_search_bar =
                    cx.new(|_| search::project_search::ProjectSearchBar::new());
                toolbar.add_item(project_search_bar, window, cx);
            });
        });
        let workspace_handle = cx.entity();
        let pane_added_languages = observe_app_state.languages.clone();
        cx.subscribe_in(&workspace_handle, window, move |_, _, event, window, cx| {
            if let workspace::Event::PaneAdded(pane) = event {
                let languages = pane_added_languages.clone();
                pane.update(cx, |pane, cx| {
                    pane.toolbar().update(cx, |toolbar, cx| {
                        let buffer_search_bar = cx.new(|cx| {
                            search::BufferSearchBar::new(Some(languages), window, cx)
                        });
                        toolbar.add_item(buffer_search_bar, window, cx);
                        let project_search_bar = cx
                            .new(|_| search::project_search::ProjectSearchBar::new());
                        toolbar.add_item(project_search_bar, window, cx);
                    });
                });
            }
        })
        .detach();

        // Custom always-on application menu bar. On Android there's no
        // native menu surface and the production `title_bar` crate pulls
        // in audio/livekit/auto_update/git_ui — too heavy for this stage.
        // The bar dispatches the same actions production's menus do, and
        // can be hidden via the chevron-up button or the
        // `ToggleAppMenuBar` action (Ctrl+Alt+M).
        let weak_workspace = cx.weak_entity();
        let menu_bar = cx.new(|inner_cx| menu_bar::MenuBar::new(weak_workspace.clone(), inner_cx));
        menu_bar::register(&menu_bar);
        // Title bar sits below the menu bar with the project name,
        // Restricted Mode trust badge, and settings chevron. The
        // chevron's tap toggles `menu_bar.hidden`; right-click /
        // two-finger tap opens the Settings/Keymap/Themes/Icon
        // Themes/Extensions dropdown.
        let title_bar = cx.new(|inner_cx| {
            title_bar::TitleBar::new(
                weak_workspace.clone(),
                menu_bar.downgrade(),
                inner_cx,
            )
        });
        let header = cx.new(|_| header::Header::new(menu_bar, title_bar));
        workspace.set_titlebar_item(header.into(), window, cx);

        // Mirror production's `initialize_panels`: every newly-constructed
        // workspace asynchronously loads each panel and attaches it.
        // Production loads project / outline / terminal / git / collab /
        // debug / agent panels in parallel; we only ship the ones whose
        // deps are already wired. The PanelButtons in the status bar
        // render one button per registered panel, so adding more panels
        // surfaces more bottom-bar buttons for free.
        let weak = cx.weak_entity();
        cx.spawn_in(window, async move |_, cx| {
            let project_panel =
                project_panel::ProjectPanel::load(weak.clone(), cx.clone());
            let outline_panel =
                outline_panel::OutlinePanel::load(weak.clone(), cx.clone());
            let terminal_panel =
                terminal_view::terminal_panel::TerminalPanel::load(weak.clone(), cx.clone());
            let git_panel =
                git_ui::git_panel::GitPanel::load(weak.clone(), cx.clone());
            let (project_panel, outline_panel, terminal_panel, git_panel) =
                futures::future::join4(
                    project_panel,
                    outline_panel,
                    terminal_panel,
                    git_panel,
                )
                .await;
            weak.update_in(cx, |workspace, window, cx| {
                if let Ok(panel) = project_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = outline_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = terminal_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = git_panel {
                    workspace.add_panel(panel, window, cx);
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    })
    .detach();

    // Production's `prompt_and_open_paths` assumes multi-window: when no
    // existing local workspace passes the `workspace_location` filter, it
    // opens a new window via `Workspace::new_local(.., None, ..)`. Single-
    // window Android rejects the second `cx.open_window`, so we replace
    // the `Open` action with the same call production makes once it's
    // already inside the right window — `MultiWorkspace::open_project`,
    // which `find_or_create_local_workspace`s and reuses the empty
    // workspace as the slot to swap. That path also dismisses any open
    // Onboarding/WelcomePage items naturally because the workspace gets
    // replaced.
    cx.on_action(|_: &workspace::Open, cx: &mut App| {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |cx| {
            let picked = match paths.await {
                Ok(Ok(Some(p))) => p,
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    error!("zed_android: Open picker failed: {err:#}");
                    return;
                }
                Err(_) => return,
            };

            // The first-launch onboarding path (show_onboarding_view →
            // workspace::open_new) opens a plain `Workspace` window, not
            // a `MultiWorkspace`. Returning-launch path is also `Workspace`
            // until something attaches the multi-workspace shell. The Open
            // action can fire on either, so try MultiWorkspace first then
            // fall through to plain Workspace; the previous code silently
            // no-op'd ("no active MultiWorkspace for Open" log + return)
            // when invoked on the welcome screen of a fresh install.
            let active = cx.update(|cx| cx.active_window());
            let Some(active) = active else {
                error!("zed_android: no active window for Open");
                return;
            };

            if let Some(mw) = active.downcast::<MultiWorkspace>() {
                let task = mw.update(cx, |mw, window, cx| {
                    mw.open_project(picked, workspace::OpenMode::Activate, window, cx)
                });
                if let Ok(task) = task {
                    if let Err(err) = task.await {
                        error!("zed_android: open_project failed: {err:#}");
                    }
                }
            } else if let Some(ws) = active.downcast::<workspace::Workspace>() {
                // Fall-through path: add the picked folder as a worktree
                // of the current workspace. The welcome page tab survives
                // alongside (user can close it). Not as polished as the
                // MultiWorkspace `find_or_create + replace` flow, but it
                // unblocks Open Project from the welcome screen on every
                // boot path.
                let task = ws.update(cx, |ws, window, cx| {
                    ws.open_paths(
                        picked,
                        workspace::OpenOptions::default(),
                        None,
                        window,
                        cx,
                    )
                });
                if let Ok(task) = task {
                    let _ = task.await;
                }
            } else {
                error!(
                    "zed_android: active window is neither Workspace nor MultiWorkspace; \
                     Open action ignored"
                );
            }
        })
        .detach();
    });

    // ImportFromSdcard action removed in L9 — the menu entry was redundant
    // with the existing SAF picker (Open / Add Folder to Project), and the
    // copy-into-`~/projects` flow it ran is now triggered from the noexec
    // banner's confirmation dialog (see title_bar.rs::render_noexec_banner)
    // when an opened worktree turns out to live on a noexec mount.

    // Mirror production zed/src/main.rs's first-launch decision. Both
    // helpers internally call `Workspace::new_local` → `cx.open_window`,
    // construct the Project, build Workspace + MultiWorkspace, and run
    // their own `init` callback. We don't need to do any of that
    // manually; `observe_new` above attaches the project panel.
    let kvp = KeyValueStore::global(cx);
    if matches!(kvp.read_kvp(onboarding::FIRST_OPEN), Ok(None)) {
        info!("zed_android: first launch → show_onboarding_view");
        onboarding::show_onboarding_view(app_state.clone(), cx)
            .detach_and_log_err(cx);
    } else {
        info!("zed_android: returning launch → workspace::open_new");
        workspace::open_new(
            Default::default(),
            app_state.clone(),
            cx,
            |_workspace, _window, _cx| {},
        )
        .detach_and_log_err(cx);
    }

    Ok(())
}
