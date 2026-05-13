//! gpui Global carrying the currently active Zdroid runtime adapter.
//! Lets the onboarding page render a reactive "Current: <adapter>"
//! label that updates the instant the user picks something in the
//! rich picker window (separate window, so file-watching + cx.notify
//! is the only way to push updates back).
//!
//! Wire diagram:
//!
//!   android_main (zed_android lib.rs)
//!     reads runtime.toml
//!     `cx.set_global(ActiveRuntime { current: Some(<id>) })`
//!
//!   user opens picker, selects something
//!     `runtime_picker::select` writes runtime.toml AND calls
//!     `cx.set_global(ActiveRuntime { current: Some(<new_id>) })`
//!
//!   Onboarding entity (created when user navigates to welcome)
//!     `_runtime_subscription = cx.observe_global::<ActiveRuntime>(
//!         |_, cx| cx.notify())`
//!     → next render re-runs `render_android_runtime_section` which
//!       reads `cx.global::<ActiveRuntime>()` and shows the fresh
//!       label.
//!
//! Lives in `onboarding` (not `zdroid_runtime`) because onboarding
//! is the consumer that needs the gpui::Global trait, and we don't
//! want to drag gpui into `zdroid_runtime` (the `zd-exec` binary
//! consumes that crate and doesn't need any UI infrastructure).

#[cfg(target_os = "android")]
use zdroid_runtime::RuntimeId;

/// Snapshot of the active runtime selection visible to gpui entities.
/// Updated from two places:
///   - `android_main` at boot when the app initializes.
///   - the runtime picker when the user selects an adapter.
///
/// On non-Android targets the `current` field is always `None` (no
/// runtime adapter concept), but the struct exists unconditionally so
/// non-Android tests / docs builds compile cleanly.
#[derive(Debug, Default, Clone)]
pub struct ActiveRuntime {
    #[cfg(target_os = "android")]
    pub current: Option<RuntimeId>,
    #[cfg(not(target_os = "android"))]
    pub current: Option<()>,
}

impl gpui::Global for ActiveRuntime {}
