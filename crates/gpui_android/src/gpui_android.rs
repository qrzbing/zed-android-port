#![cfg(target_os = "android")]
//! Android backend for the GPUI Platform trait.

mod dispatcher;
mod display;
mod events;
mod keyboard;
mod platform;
mod window;

pub use platform::AndroidPlatform;

use std::rc::Rc;

/// Run a gpui application backed by Android's GameActivity event loop.
///
/// Call this from your `android_main` no_mangle entry point. It constructs
/// the `AndroidPlatform`, hands it to `gpui::Application`, and drives the
/// run loop. Replaces the desktop/web `gpui_platform::application()` path
/// because we need to thread the `AndroidApp` through Platform construction.
pub fn run<F>(android_app: android_activity::AndroidApp, on_finish_launching: F)
where
    F: 'static + FnOnce(&mut gpui::App),
{
    let platform: Rc<dyn gpui::Platform> = Rc::new(AndroidPlatform::new(android_app, false));
    let app = gpui::Application::with_platform(platform);
    app.run(on_finish_launching);
}
