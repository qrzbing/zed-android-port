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
use gpui::{App, AppContext as _};
use language::{Buffer, Language, LanguageConfig};
use log::{error, info};
use multi_buffer::MultiBuffer;

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

    cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
        let buffer = cx.new(|cx| Buffer::local(text.clone(), cx).with_language(rust.clone(), cx));
        let multibuffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        cx.new(|cx| Editor::new(EditorMode::full(), multibuffer, None, window, cx))
    })?;

    info!("zed_android: editor window opened");
    Ok(())
}
