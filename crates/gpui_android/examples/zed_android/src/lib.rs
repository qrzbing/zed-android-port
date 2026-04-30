#![cfg(target_os = "android")]
//! Zed Workspace running on Android. Boots up the full client/project/
//! workspace stack and shows the WelcomePage on first launch (no auto-
//! opened project) — matches official Zed's first-run behaviour.

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
use project::{LocalProjectFlags, Project};
use session::{AppSession, Session};
use gpui::{App, AppContext as _};
use log::{error, info};
use workspace::{AppState, MultiWorkspace, Workspace, WorkspaceStore};
use reqwest_client::ReqwestClient;

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

    // Android sandboxes don't expose a home directory; many Zed code paths
    // call `dirs::home_dir()` which `expect`s and panics here. Pointing HOME
    // at our app-private data dir gives every consumer a writable, persistent
    // location without further code changes.
    // SAFETY: set_var is sound before any other thread runs; android_main is
    // the very first user code on the main thread.
    unsafe {
        std::env::set_var("HOME", &data_path);
    }

    gpui_android::run(app, assets::Assets, move |cx: &mut App| {
        if let Err(err) = boot(cx, &data_path) {
            error!("zed_android: boot failed: {err:#}");
        }
    });
}

fn boot(cx: &mut App, data_path: &std::path::Path) -> Result<()> {
    paths::set_custom_data_dir(&data_path.to_string_lossy());
    info!("zed_android: paths data_dir set");

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
    let node_runtime = NodeRuntime::unavailable();
    info!("zed_android: RealFs + NodeRuntime::unavailable constructed");

    cx.text_system()
        .add_fonts(vec![Cow::Borrowed(BUNDLED_FONT)])?;

    let language_registry =
        Arc::new(language::LanguageRegistry::new(cx.background_executor().clone()));

    let registry = theme::ThemeRegistry::global(cx);
    info!(
        "zed_android: theme registry has {} themes loaded",
        registry.list().len()
    );

    let app_session = cx.new(|cx| AppSession::new(session, cx));
    let user_store_for_project = user_store.clone();
    let languages_for_project = language_registry.clone();
    let fs_for_project = fs.clone();
    let node_for_project = node_runtime.clone();
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

    client::init(&client, cx);
    Project::init(&client, cx);
    workspace::init(app_state.clone(), cx);
    command_palette::init(cx);
    search::init(cx);
    vim::init(cx);
    project_panel::init(cx);
    settings_ui::init(cx);
    info!("zed_android: client/Project/workspace/command_palette/search/vim/project_panel/settings_ui init complete");

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

    let project = Project::local(
        client.clone(),
        node_for_project,
        user_store_for_project,
        languages_for_project,
        fs_for_project,
        None,
        LocalProjectFlags::default(),
        cx,
    );
    info!("zed_android: Project::local constructed (entity_id={:?})", project.entity_id());

    let project_for_window = project.clone();
    cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
        let workspace = cx.new(|cx| {
            Workspace::new(None, project_for_window.clone(), app_state.clone(), window, cx)
        });
        // No items, no worktrees: the center pane's `should_display_welcome_page`
        // is true (set inside Workspace::new), so the WelcomePage with
        // "Get Started" + "Recent Projects" surfaces on first launch.
        // Still attach the project panel so when a worktree opens later
        // (via "Open Project" / a recent), the file tree is in place.
        workspace.update(cx, |_, cx| {
            let weak = cx.weak_entity();
            cx.spawn_in(window, async move |_, cx| {
                match project_panel::ProjectPanel::load(weak.clone(), cx.clone()).await {
                    Ok(panel) => {
                        if let Err(err) = weak.update_in(cx, |workspace, window, cx| {
                            workspace.add_panel(panel, window, cx);
                        }) {
                            error!("zed_android: add_panel failed: {err:#}");
                        } else {
                            info!("zed_android: project_panel attached");
                        }
                    }
                    Err(err) => error!("zed_android: ProjectPanel::load failed: {err:#}"),
                }
            })
            .detach();
        });
        // Wrap in MultiWorkspace so its render establishes the "Workspace"
        // KeyContext on the dispatch tree. Without this, every
        // `"context": "Workspace"` keymap binding (Ctrl+Alt+B for
        // ToggleRightDock, Ctrl+B for left dock, etc.) silently misses.
        cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
    })?;

    info!("zed_android: workspace window opened");
    Ok(())
}
