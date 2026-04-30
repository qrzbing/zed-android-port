#![cfg(target_os = "android")]
//! Real Zed `Editor` running on Android. Bundles the Rust tree-sitter
//! highlight query and Lilex font, opens `/sdcard/Documents/test.rs`, and
//! renders the editor through the gpui_android backend. No project, LSP, or
//! workspace — just the editor element with tree-sitter highlights.

use std::borrow::Cow;
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
    theme_settings::init(theme::LoadThemes::JustBase, cx);
    editor::init(cx);

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

    let buffer = cx.new(|cx| Buffer::local(text, cx).with_language(rust.clone(), cx));
    let multibuffer = cx.new(|cx| MultiBuffer::singleton(buffer.clone(), cx));

    // Wire ctrl-s → SaveFile. With `project: None` the editor's normal save
    // path is unwired, so we own the persistence. Logs both the dispatch
    // (was the action even routed here?) and the write outcome.
    cx.bind_keys([KeyBinding::new("ctrl-s", SaveFile, None)]);
    let buffer_for_save = buffer.clone();
    cx.on_action(move |_: &SaveFile, cx: &mut App| {
        info!("zed_android: SaveFile action fired");
        let text = buffer_for_save.read(cx).text();
        match std::fs::write(TARGET_PATH, &text) {
            Ok(()) => info!("zed_android: saved {} bytes to {TARGET_PATH}", text.len()),
            Err(err) => error!("zed_android: save failed: {err:#}"),
        }
    });

    cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
        cx.new(|cx| Editor::new(EditorMode::full(), multibuffer, None, window, cx))
    })?;

    info!("zed_android: editor window opened");
    Ok(())
}
