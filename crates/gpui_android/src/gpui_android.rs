#![cfg(target_os = "android")]
//! Android backend for the GPUI Platform trait.

pub mod askpass_install;
mod captured_pointer;
mod clipboard;
mod cursor;
mod dispatcher;
mod display;
pub mod dns_bridge;
mod events;
mod frame_timing;
mod keyboard;
mod multi_window;
mod platform;
mod saf;
pub mod storage;
pub mod termux_bootstrap;
mod window;
pub mod zd_exec_install;

pub use platform::AndroidPlatform;

use std::rc::Rc;

/// Run a gpui application backed by Android's GameActivity event loop.
///
/// Call this from your `android_main` no_mangle entry point. It constructs
/// the `AndroidPlatform`, hands it to `gpui::Application`, and drives the
/// run loop. Replaces the desktop/web `gpui_platform::application()` path
/// because we need to thread the `AndroidApp` through Platform construction.
///
/// `assets` is the source the SVG renderer uses to resolve `icons/...`,
/// `images/...`, and any other asset paths Zed's UI references. Pass
/// `assets::Assets` (the bundled RustEmbed source) for the typical case;
/// without an asset source icons render as blank rectangles.
pub fn run<A, F>(android_app: android_activity::AndroidApp, assets: A, on_finish_launching: F)
where
    A: gpui::AssetSource,
    F: 'static + FnOnce(&mut gpui::App),
{
    let platform: Rc<dyn gpui::Platform> = Rc::new(AndroidPlatform::new(android_app, false));
    let app = gpui::Application::with_platform(platform).with_assets(assets);
    app.run(on_finish_launching);
}
