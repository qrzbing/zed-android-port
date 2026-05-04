#![cfg(target_os = "android")]
//! Zed Workspace running on Android. Boots up the full client/project/
//! workspace stack and shows the WelcomePage on first launch (no auto-
//! opened project) — matches official Zed's first-run behaviour.

mod header;
mod menu_bar;
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
use gpui::{App, AppContext as _, UpdateGlobal as _, actions};
use log::{error, info};
use settings::Settings as _;
use workspace::{AppState, MultiWorkspace, Workspace, WorkspaceStore};
use reqwest_client::ReqwestClient;

actions!(
    zed_android,
    [
        /// Pick a tree from shared storage, recursively copy it to
        /// ~/projects/<name>, and open the local copy. Source on /sdcard
        /// is left untouched. The local copy lives on app-private storage
        /// where exec is allowed, so cargo / go / make / native build
        /// tools all run natively without any noexec workaround.
        ImportFromSdcard
    ]
);

fn minimal_window_options(_: Option<uuid::Uuid>, _cx: &mut App) -> gpui::WindowOptions {
    gpui::WindowOptions::default()
}


const BUNDLED_FONT: &[u8] =
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Regular.ttf");

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
        .unwrap_or_else(|| PathBuf::from("/data/data/dev.zed.zed_android/files"));
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
    // PATH is set to `$PREFIX/bin:$PATH` IFF the bootstrap is on disk —
    // detected by the presence of `$PREFIX/bin/bash`. Pre-bootstrap we'd
    // override PATH with a non-existent directory and any subprocess
    // looking up `git`/`grep`/etc. would fail. Post-bootstrap we want
    // every Zed subprocess (git status, LSP spawns, terminal) to find
    // Termux binaries first and fall back to `/system/bin/*` only for
    // things bionic provides natively.
    static ENV_INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = ENV_INITIALIZED.get_or_init(|| {
        let prefix = data_path.join("usr");
        let termux_home = data_path.join("home");
        let bash_present = prefix.join("bin/bash").is_file();
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
            std::env::set_var("TERMUX_APP__PACKAGE_NAME", "dev.zed.zed_android");
            std::env::set_var("TMPDIR", prefix.join("tmp"));
            std::env::set_var("TERM", "xterm-256color");
            std::env::set_var("LANG", "en_US.UTF-8");
            std::env::set_var("COLORTERM", "truecolor");
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
            if bash_present {
                let prefix_bin = prefix.join("bin");
                let existing = std::env::var_os("PATH").unwrap_or_default();
                let mut new_path = std::ffi::OsString::from(&prefix_bin);
                new_path.push(":");
                new_path.push(&existing);
                std::env::set_var("PATH", &new_path);
                std::env::set_var("SHELL", prefix.join("bin/bash"));

                // DELIBERATELY NOT SET: LD_LIBRARY_PATH.
                //
                // Our bootstrap binaries (built with TERMUX_APP_PACKAGE=
                // dev.zed.zed_android) have DT_RUNPATH pointing at our
                // real lib path, so they load libs natively without help.
                //
                // Setting LD_LIBRARY_PATH globally poisons every spawned
                // subprocess, including Android system processes — e.g.
                // /system/bin/app_process64 loads /system/lib64/libsqlite.so
                // which needs OpenSSL_add_all_algorithms; the linker
                // searches LD_LIBRARY_PATH first, finds our OpenSSL 3.x
                // libssl.so (which dropped that deprecated symbol),
                // CANNOT LINK, cascading into JVM stack overflow when
                // ART retries the failing dlopen. Crashed copy/paste from
                // the terminal panel because of exactly this.
                //
                // Upstream Termux packages we install via `pkg` DO need
                // LD_LIBRARY_PATH to find their libs at runtime, but we
                // set it per-spawn (in the LSP launcher / terminal-panel
                // pty bringup), not as a global env var.
                log::info!(
                    "zed_android: PATH prefixed with {} (bootstrap detected)",
                    prefix_bin.display()
                );
            }
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
        gpui_android::termux_bootstrap::apply_runtime_patches(&data_path)
    {
        log::warn!(
            "zed_android: termux runtime patches failed: {err:#}; \
             upstream `pkg install` of packages with hardcoded shebangs \
             may need a manual sed + dpkg --configure -a"
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

    let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
    let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));
    info!("zed_android: UserStore + WorkspaceStore constructed");

    let fs: Arc<dyn Fs> = Arc::new(RealFs::new(None, cx.background_executor().clone()));
    <dyn Fs>::set_global(fs.clone(), cx);
    let node_runtime = NodeRuntime::unavailable();
    info!("zed_android: RealFs + NodeRuntime::unavailable constructed");

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

    cx.text_system()
        .add_fonts(vec![Cow::Borrowed(BUNDLED_FONT)])?;

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
        fs,
        build_window_options: minimal_window_options,
        node_runtime,
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

    // Default `terminal.shell` to $PREFIX/bin/bash once L2a's bootstrap is
    // on disk. Only writes if the user hasn't already chosen a shell —
    // explicit user choices (System, a different program, etc.) win. The
    // first launch *with* the bootstrap mutates the user settings file
    // exactly once; subsequent launches see Some(...) and short-circuit.
    let bash_path = data_path.join("usr/bin/bash");
    if bash_path.is_file() {
        let bash_str = bash_path.to_string_lossy().to_string();
        let fs_for_settings = app_state.fs.clone();
        cx.global::<settings::SettingsStore>().update_settings_file(
            fs_for_settings,
            move |content, _cx| {
                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                if terminal.project.shell.is_none() {
                    terminal.project.shell =
                        Some(settings::Shell::Program(bash_str.clone()));
                    log::info!(
                        "zed_android: defaulted terminal.shell to {} (bootstrap detected)",
                        bash_str
                    );
                }
            },
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
    diagnostics::init(cx);
    workspace::init(app_state.clone(), cx);
    command_palette::init(cx);
    search::init(cx);
    // Mirror production zed/src/main.rs:710. Without this global the
    // in-buffer search bar inside each pane never has a search-bar
    // entity registered on its toolbar — Ctrl-F shows nothing.
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
    keymap_editor::init(cx);
    inspector_ui::init(app_state.clone(), cx);
    json_schema_store::init(cx);
    recent_projects::init(cx);
    which_key::init(cx);
    settings_ui::init(cx);
    terminal_view::init(cx);
    // Onboarding reads `AllAgentServersSettings` from the SettingsStore;
    // `SettingsStore::get` panics if the type isn't registered, so register
    // it before any onboarding render. agent_settings doesn't expose a
    // separate init function — registering the type is the whole step.
    project::agent_server_store::AllAgentServersSettings::register(cx);
    onboarding::init(cx);
    menu_bar::register_actions(cx);
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
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };

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
            let mw = cx.update(|cx| {
                cx.active_window()
                    .and_then(|w| w.downcast::<MultiWorkspace>())
            });
            let Some(mw) = mw else {
                error!("zed_android: no active MultiWorkspace for Open");
                return;
            };
            let task = mw.update(cx, |mw, window, cx| {
                mw.open_project(picked, workspace::OpenMode::Activate, window, cx)
            });
            if let Ok(task) = task {
                if let Err(err) = task.await {
                    error!("zed_android: open_project failed: {err:#}");
                }
            }
        })
        .detach();
    });

    let projects_root = termux_home.join("projects");
    cx.on_action(move |_: &ImportFromSdcard, cx: &mut App| {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let projects_root = projects_root.clone();
        cx.spawn(async move |cx| {
            let picked = match paths.await {
                Ok(Ok(Some(p))) if !p.is_empty() => p,
                Ok(Ok(None)) | Ok(Ok(Some(_))) => return,
                Ok(Err(err)) => {
                    error!(
                        "zed_android: ImportFromSdcard picker failed: {err:#}"
                    );
                    return;
                }
                Err(_) => return,
            };
            let src = picked.into_iter().next().expect("non-empty picked");
            let basename = match src.file_name() {
                Some(n) => n.to_owned(),
                None => {
                    error!(
                        "zed_android: ImportFromSdcard: picked path {} has no file name",
                        src.display()
                    );
                    return;
                }
            };
            let mut dst = projects_root.join(&basename);
            // Don't clobber an existing project with the same name.
            // Suffix `-imported`, `-imported-2`, etc. so the user keeps
            // their previous import and the new one lands cleanly.
            if dst.exists() {
                let stem = basename.to_string_lossy().to_string();
                let mut suffix = 1usize;
                loop {
                    let candidate = projects_root.join(format!(
                        "{stem}-imported{}",
                        if suffix == 1 {
                            String::new()
                        } else {
                            format!("-{suffix}")
                        }
                    ));
                    if !candidate.exists() {
                        dst = candidate;
                        break;
                    }
                    suffix += 1;
                }
            }
            info!(
                "zed_android: ImportFromSdcard: copying {} -> {}",
                src.display(),
                dst.display()
            );
            let dst_for_copy = dst.clone();
            let copy_result = cx
                .background_spawn(async move {
                    gpui_android::storage::copy_tree(&src, &dst_for_copy)
                })
                .await;
            match copy_result {
                Ok(bytes) => info!(
                    "zed_android: ImportFromSdcard: copied {bytes} bytes to {}",
                    dst.display()
                ),
                Err(err) => {
                    error!(
                        "zed_android: ImportFromSdcard: copy failed: {err:#}"
                    );
                    return;
                }
            }
            let mw = cx.update(|cx| {
                cx.active_window()
                    .and_then(|w| w.downcast::<MultiWorkspace>())
            });
            let Some(mw) = mw else {
                error!(
                    "zed_android: ImportFromSdcard: no active MultiWorkspace"
                );
                return;
            };
            let task = mw.update(cx, |mw, window, cx| {
                mw.open_project(
                    vec![dst],
                    workspace::OpenMode::Activate,
                    window,
                    cx,
                )
            });
            if let Ok(task) = task {
                if let Err(err) = task.await {
                    error!(
                        "zed_android: ImportFromSdcard: open_project failed: {err:#}"
                    );
                }
            }
        })
        .detach();
    });

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
