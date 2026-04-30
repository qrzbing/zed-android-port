#![cfg(target_os = "android")]
//! Real Zed `Editor` running on Android. Bundles the Rust tree-sitter
//! highlight query and Lilex font, opens `/sdcard/Documents/test.rs`, and
//! renders the editor through the gpui_android backend. No project, LSP, or
//! workspace — just the editor element with tree-sitter highlights.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;

use android_activity::AndroidApp;
use anyhow::Result;
use editor::{Editor, EditorMode};
use gpui::{App, AppContext as _, KeyBinding, actions};
use language::{Buffer, Language, LanguageConfig};
use log::{error, info};
use multi_buffer::MultiBuffer;

actions!(zed_android, [SaveFile]);

const BUNDLED_FONT: &[u8] =
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Regular.ttf");

const RUST_HIGHLIGHTS: &str = include_str!("../../../../grammars/src/rust/highlights.scm");

const TARGET_PATH: &str = "/sdcard/Documents/test.rs";

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("zed_android"),
    );
    info!("zed_android: android_main entry");

    gpui_android::run(app, |cx: &mut App| {
        if let Err(err) = boot(cx) {
            error!("zed_android: boot failed: {err:#}");
        }
    });
}

fn boot(cx: &mut App) -> Result<()> {
    info!("zed_android: settings + theme + editor init");
    settings::init(cx);
    theme_settings::init(theme::LoadThemes::All(Box::new(assets::Assets)), cx);
    editor::init(cx);

    let registry = theme::ThemeRegistry::global(cx);
    info!(
        "zed_android: theme registry has {} themes loaded",
        registry.list().len()
    );

    // Load the default keymap so backspace/delete/arrows/ctrl-shortcuts route
    // to editor actions. `_allow_partial_failure` skips bindings whose actions
    // aren't registered yet (vim::*, terminal::*, workspace::* until 8.3) —
    // we get the editor::* and zed::* subset and the rest land later.
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

    cx.text_system()
        .add_fonts(vec![Cow::Borrowed(BUNDLED_FONT)])?;

    let rust = Arc::new(
        Language::new(
            LanguageConfig {
                name: "Rust".into(),
                ..Default::default()
            },
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_highlights_query(RUST_HIGHLIGHTS)?,
    );

    let text = std::fs::read_to_string(TARGET_PATH).unwrap_or_else(|err| {
        error!("zed_android: failed to read {TARGET_PATH}: {err:#}");
        format!("// {TARGET_PATH} unavailable: {err}\n")
    });
    info!("zed_android: loaded {} bytes from {TARGET_PATH}", text.len());

    // Construct the Buffer/MultiBuffer/Editor inside `open_window` so that
    // gpui's update guard governs the borrow lifetime — doing it on the bare
    // `&mut App` lets `with_language`'s background tree-sitter task fire
    // re-entrant `borrow_mut` calls and panic. The save handler reaches the
    // buffer through this shared cell, written from inside the update.
    let buffer_slot: Rc<OnceCell<gpui::Entity<Buffer>>> = Rc::new(OnceCell::new());

    cx.bind_keys([KeyBinding::new("ctrl-s", SaveFile, None)]);
    let buffer_for_save = buffer_slot.clone();
    cx.on_action(move |_: &SaveFile, cx: &mut App| {
        info!("zed_android: SaveFile action fired");
        let Some(buffer) = buffer_for_save.get() else {
            error!("zed_android: SaveFile fired before buffer initialized");
            return;
        };
        let text = buffer.read(cx).text();
        match std::fs::write(TARGET_PATH, &text) {
            Ok(()) => info!("zed_android: saved {} bytes to {TARGET_PATH}", text.len()),
            Err(err) => error!("zed_android: save failed: {err:#}"),
        }
    });

    let buffer_for_window = buffer_slot.clone();
    cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
        let buffer =
            cx.new(|cx| Buffer::local(text.clone(), cx).with_language(rust.clone(), cx));
        let _ = buffer_for_window.set(buffer.clone());
        let multibuffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        cx.new(|cx| Editor::new(EditorMode::full(), multibuffer, None, window, cx))
    })?;

    info!("zed_android: editor window opened");
    Ok(())
}
