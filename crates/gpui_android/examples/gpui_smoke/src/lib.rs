#![cfg(target_os = "android")]
//! Minimal gpui smoke binary on Android. Renders a solid blue full-screen
//! background through the actual `Platform`/`Window`/`Renderer` traits — proves
//! the gpui_android backend wires up end to end. No editor yet.

use android_activity::AndroidApp;
use gpui::{App, AppContext as _, Context, IntoElement, Render, Styled, Window, div, rgb};
use log::info;

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("gpui_smoke"),
    );
    info!("gpui_smoke: android_main entry");

    gpui_android::run(app, |cx: &mut App| {
        info!("gpui_smoke: on_finish_launching");
        if let Err(err) = cx.open_window(gpui::WindowOptions::default(), |_window, cx| {
            cx.new(|_| RootView)
        }) {
            log::error!("gpui_smoke: open_window failed: {err:#}");
        } else {
            info!("gpui_smoke: window opened");
        }
    });
}

struct RootView;

impl Render for RootView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x224488))
    }
}
