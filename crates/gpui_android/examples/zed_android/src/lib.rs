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
use zdroid_runtime::{
    RuntimeId, RuntimeProvider,
    adapters,
    config::RuntimeFile,
};

fn minimal_window_options(_: Option<uuid::Uuid>, _cx: &mut App) -> gpui::WindowOptions {
    gpui::WindowOptions::default()
}

/// Initialize `rustls-platform-verifier` against the Android trust store.
/// Required before any in-process TLS verification, since the verifier
/// reaches into the JVM via the bundled `rustls-platform-verifier-android`
/// `.aar` (wired into Gradle in `android/settings.gradle.kts`). Without
/// this, every rustls handshake fails with `UnknownIssuer`.
///
/// Idempotent: `init_with_env` is OnceCell-gated internally, so the
/// re-entrant `android_main` path on activity recreation is safe.
fn init_platform_tls(android_app: &AndroidApp) {
    use jni::{JavaVM, objects::JObject};

    let result = (|| -> anyhow::Result<()> {
        // SAFETY: vm_as_ptr and activity_as_ptr are valid for the
        // lifetime of the process from android_main onward; the Android
        // runtime owns both. Same pattern as `dns_bridge::query_android_dns`
        // and `clipboard.rs`.
        unsafe {
            let vm = JavaVM::from_raw(android_app.vm_as_ptr().cast())?;
            let mut env = vm.attach_current_thread()?;
            let activity = JObject::from_raw(android_app.activity_as_ptr().cast());
            rustls_platform_verifier::android::init_with_env(&mut env, activity)?;
        }
        Ok(())
    })();
    if let Err(err) = result {
        log::error!("zed_android: rustls_platform_verifier init failed: {err:#}");
    } else {
        log::info!("zed_android: rustls_platform_verifier initialized");
    }
}

/// Build the active adapter from `runtime.toml`, returning the boxed
/// provider so callers can ask for its `environment_root()` and
/// adapter-derived metadata. Returns None when no toml exists or the
/// adapter construction fails (defaults are filled in by the picker
/// the first time the user opens it).
fn build_active_provider(
    data_path: &std::path::Path,
) -> Option<Box<dyn RuntimeProvider>> {
    let file = RuntimeFile::load(&data_path.join("usr/etc/zd-runtime.toml"))
        .ok()
        .flatten()?;
    let resolved = file.resolve().ok()?;
    adapters::for_config(&resolved).ok()
}

/// Same as `build_active_provider` but never returns `None`. When no
/// runtime.toml exists yet (first launch before the picker is opened)
/// we fall back to a default Bootstrap adapter so the env-init path
/// always has someone to ask. The user's eventual picker selection
/// rewrites runtime.toml and the next launch picks up the chosen
/// adapter via `build_active_provider`.
fn active_provider(data_path: &std::path::Path) -> Box<dyn RuntimeProvider> {
    if let Some(provider) = build_active_provider(data_path) {
        return provider;
    }
    let file = RuntimeFile::with_defaults(RuntimeId::Bootstrap);
    let resolved = file
        .resolve()
        .expect("default Bootstrap RuntimeFile must resolve");
    adapters::for_config(&resolved)
        .expect("default Bootstrap adapter must construct")
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

    init_platform_tls(&app);

    let data_path = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/data/com.zdroid/files"));
    info!("zed_android: data_path = {}", data_path.display());

    // The Zed-Rust process env is now adapter-owned. Every set_var/
    // remove_var that used to live inline here is produced by the
    // active runtime adapter's `env_for_zed_process`:
    //
    //   - Chroot adapter ships a bionic-clean env (no PREFIX, no
    //     TERMUX__*, no libtermux-exec preload). PATH front-loaded with
    //     the zd-runtime symlink farm so `Command::new("java")` finds
    //     zd-exec → zd-spawnd → chroot dispatch.
    //
    //   - Bootstrap adapter keeps the historical Termux-flavored env
    //     so dpkg patches, apt Post-Invoke hooks, and bootstrap-side
    //     tooling keep working unchanged.
    //
    //   - External Termux returns a minimal bionic env; spawns route
    //     via Intent (Phase 7) so nothing here leaks across.
    //
    // The active provider also publishes a terminal env overlay that
    // `crates/terminal/src/terminal.rs` applies when the PTY spawns,
    // since alacritty wipes inherited env on bringup.
    //
    // SAFETY: `set_var` / `remove_var` mutate libc-shared process
    // state. The invariant is "no other thread reads/writes libc env
    // via getenv/setenv at this point" — JVM service threads exist by
    // android_main but don't touch libc env. OnceLock makes activity-
    // recreation re-entry a deterministic no-op.
    //
    // DELIBERATELY NOT SET: LD_LIBRARY_PATH. Setting it globally
    // poisons every spawned subprocess (including /system/bin/
    // app_process64 trying to load /system/lib64/libsqlite.so against
    // our OpenSSL 3.x); Upstream Termux packages that need it set it
    // per-spawn in their launcher scripts. ZED_RELEASE_CHANNEL is also
    // deliberately not flipped to "nightly": channel switching
    // namespaces Zed's app data dir and would shadow the user's
    // existing settings. The dev-channel hard-bail in
    // `crates/remote/src/transport/ssh.rs` is patched directly.
    let provider = active_provider(&data_path);
    log::info!("zed_android: runtime adapter = {:?}", provider.id());

    static ENV_INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = ENV_INITIALIZED.get_or_init(|| {
        let ops = provider.env_for_zed_process(&data_path);
        unsafe {
            for (key, op) in &ops {
                match op {
                    util::env::EnvOp::Set(value) => std::env::set_var(key, value),
                    util::env::EnvOp::Remove => std::env::remove_var(key),
                }
            }
        }
        log::info!(
            "zed_android: PATH = {}; SHELL = {}",
            std::env::var("PATH").unwrap_or_default(),
            std::env::var("SHELL").unwrap_or_default(),
        );
    });

    // Publish the per-adapter terminal env overlay so
    // `crates/terminal/src/terminal.rs` can apply it when the PTY
    // spawns. Idempotent: subsequent android_main re-entries (activity
    // recreation) get the same overlay; the registration is OnceLock-
    // guarded inside util::env.
    util::env::register_terminal_env_overlay(provider.env_for_terminal(&data_path));

    // Publish adapter-specific filesystem hints for editor code that
    // historically read TERMUX__HOME / TERMUX__PREFIX env vars
    // directly. Now those readers (workspace::welcome,
    // node_runtime::node_runtime, gpui_android::storage) ask the
    // active adapter via these registry slots — Bootstrap publishes
    // its Termux-flavored paths, chroot publishes the bionic-clean
    // equivalents, external Termux publishes None.
    util::env::register_workspace_root(provider.workspace_root(&data_path));
    util::env::register_npm_libtermux_exec_path(
        provider.npm_libtermux_exec_path(&data_path),
    );

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

    // Install zd-exec into <data>/files/bin/ from the APK-bundled
    // asset. Idempotent (skipped when on-disk byte length matches the
    // asset's), so it runs every boot but is essentially free past
    // the first launch. Without this, fresh installs land with no
    // zd-exec and `terminal.shell` (which the chroot adapter sets to
    // `<data>/files/bin/zd-exec`) fails with ENOENT. Phase 4 of the
    // Termux-divestment refactor moved this off `$PREFIX/bin/`.
    if let Err(err) =
        gpui_android::zd_exec_install::ensure_installed(&app, &data_path)
    {
        log::warn!(
            "zed_android: zd-exec install failed: {err:#}; \
             chroot adapter's integrated terminal will fail to spawn"
        );
    }

    // Populate <data>/files/zd-runtime/ with symlinks for every binary
    // the active runtime adapter advertises. Each symlink points at
    // zd-exec; kernel PATH lookup intercepts Zed's
    // `Command::new("java")` and routes through the bridge to wherever
    // the binary actually lives. The adapter inspects its OWN
    // filesystem (chroot walks the rootfs's /usr/bin etc.; bootstrap
    // walks $PREFIX/bin) — no hardcoded list anywhere. apt-installing
    // a new tool inside the chroot makes it show up after the next
    // launch; switching adapters rewrites the set to match the new
    // env (stale entries get swept). Pre-Phase-4 this lived at
    // `$PREFIX/zd-runtime/`.
    if let Some(provider) = build_active_provider(&data_path) {
        let binaries = provider.list_binaries();
        log::info!(
            "zed_android: zd-runtime: {} binaries from {:?} adapter",
            binaries.len(),
            provider.id(),
        );
        if let Err(err) =
            gpui_android::zd_exec_install::ensure_runtime_symlinks(&data_path, &binaries)
        {
            log::warn!(
                "zed_android: zd-runtime symlinks failed: {err:#}; \
                 LSPs that resolve binaries via PATH will fail to spawn"
            );
        }
    } else {
        log::info!(
            "zed_android: zd-runtime: no active adapter; PATH interception \
             skipped (runtime picker will set one up after first selection)"
        );
    }

    // Wire askpass to the standalone helper. Must happen BEFORE any
    // AskPassSession is created (Open Remote, git auth prompts, etc.)
    // — the askpass crate's ASKPASS_PROGRAM OnceLock initializes on
    // first read with current_exe() (= /system/bin/app_process64 on
    // Android) and subsequent set_program calls are silently ignored.
    let askpass_path = match gpui_android::askpass_install::ensure_installed(&app, &data_path) {
        Ok(path) => path,
        Err(err) => {
            log::warn!(
                "zed_android: askpass helper install failed: {err:#}; \
                 SSH password / passphrase prompts will fall back to \
                 current_exe() (= app_process64) and SIGABRT on Android"
            );
            // Construct the expected path anyway so set_program isn't
            // skipped — if the binary materializes later (next boot
            // after the install issue is resolved) it'll be picked up.
            data_path.join("zed-askpass-helper")
        }
    };
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
        // Each runtime adapter (chroot / bootstrap / external Termux)
        // gets a FULLY ISOLATED Zed install: its own config, keymap,
        // themes, sqlite db, language servers, extensions, history,
        // logs — everything. Switching adapters via the runtime picker
        // is a workspace switch the way booting from a different SSD
        // is a workspace switch: nothing bleeds across.
        //
        // We do this by setting `paths::set_custom_data_dir` to the
        // active adapter's `environment_root` instead of the generic
        // app data dir. Every `paths::*` function — `config_dir`,
        // `database_dir`, `logs_dir`, `languages_dir`,
        // `extensions_dir`, the lot — derives from this single root.
        // Zed never has to know about adapters.
        //
        // First launch (no runtime.toml yet) falls back to the plain
        // app data dir so the runtime picker UI itself can render and
        // the user can pick an adapter. After they pick + reboot, this
        // branch goes through `build_active_provider` and lands them
        // in the per-adapter root.
        let root = if let Some(provider) = build_active_provider(data_path) {
            let env_root = provider.environment_root();
            log::info!(
                "zed_android: data_dir for {:?} adapter -> {}",
                provider.id(),
                env_root.display(),
            );
            // Register the env_root with util::command so absolute-path
            // spawns under it get rewritten to route through `zd-exec`.
            // Without this, Zed exec's binaries that live inside the
            // adapter's filesystem (e.g. extension-shipped glibc proxies
            // like `java-lsp-proxy`) directly on bionic and fails with
            // ENOENT — the binary's PT_INTERP doesn't exist on host.
            // See `util::command::env_root_program_path` for the full
            // rationale; tl;dr the bridge is what makes "the env is
            // truly isolated" actually true for absolute-path spawns,
            // not just PATH-resolved ones.
            util::command::register_environment_root(env_root.clone());
            env_root.to_string_lossy().into_owned()
        } else {
            log::info!(
                "zed_android: no runtime.toml; using bare app data_dir {} \
                 (the runtime picker will set up per-adapter roots after first selection)",
                data_path.display(),
            );
            data_path.to_string_lossy().into_owned()
        };
        paths::set_custom_data_dir(&root);
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

    // Seed the `onboarding::runtime_global::ActiveRuntime` global with
    // the active adapter so the onboarding page's "Current: <adapter>"
    // label renders correctly on first show. The runtime picker
    // mutates this global on every Select (see runtime_picker.rs),
    // so subsequent changes propagate via `cx.observe_global` in
    // `Onboarding::new`.
    let provider_for_global = active_provider(data_path);
    cx.set_global(onboarding::runtime_global::ActiveRuntime {
        current: Some(provider_for_global.id()),
    });

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
    // we re-derive the spawn target every launch by asking the active
    // adapter via `RuntimeProvider::terminal_shell` so flipping the
    // adapter actually changes where the integrated terminal lands.
    //
    // Why overwrite even when the user already has a shell set: the
    // picker IS the user's terminal-target choice in this app —
    // picking chroot means "I want the terminal in the chroot's bash".
    // Honoring a stale settings.shell from a prior adapter would
    // silently contradict the picker. A user who wants something
    // custom inside the chroot configures it inside the chroot (their
    // `$SHELL`, `chsh`, etc.), not at the alacritty entry point.
    //
    // alacritty's `pw_shell` lookup on Android returns /system/bin/sh
    // (the parody) so this explicit Shell::Program is what makes any
    // useful terminal work at all.
    let provider = active_provider(data_path);
    let shell_path = provider
        .terminal_shell(data_path)
        .unwrap_or_else(|| data_path.join("usr/bin/bash"));
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
