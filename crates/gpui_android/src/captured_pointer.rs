//! Pointer-capture probe sink.
//!
//! `MainActivity.kt` calls `decorView.requestPointerCapture()` when the
//! window has focus and at least one indirect pointer (mouse / trackpad)
//! is connected. Captured `MotionEvent`s bypass Android's built-in
//! touchpad gesture detection and palm filtering and are delivered to
//! the registered `OnCapturedPointerListener`. The listener stringifies
//! each event and calls `NativeBridge.nativeOnCapturedPointerProbe`,
//! which lands here.
//!
//! For the probe pass we only log. Once we've read what Samsung Book
//! Cover Keyboard's trackpad actually emits at the raw layer (whether
//! pointer_count goes above 1 for two-finger gestures, whether
//! `AXIS_RELATIVE_X/Y` carry the deltas, whether scroll arrives as
//! `ACTION_SCROLL` or stays as `HOVER_MOVE` with relative axes), we
//! design the proper synthesis layer.

use jni::objects::{JObject, JString};

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeOnCapturedPointerProbe<
    'local,
>(
    mut env: jni::JNIEnv<'local>,
    _bridge: JObject<'local>,
    summary: JString<'local>,
) {
    let summary: String = match env.get_string(&summary) {
        Ok(s) => s.into(),
        Err(err) => {
            log::warn!("captured_pointer: failed to decode summary: {err:#}");
            return;
        }
    };
    log::info!("captured_pointer: {summary}");
}
