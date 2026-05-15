//! Bridge between gpui's first-paint event and the Android SplashScreen
//! API. MainActivity's `installSplashScreen()` registers a
//! `setKeepOnScreenCondition` that polls `nativeIsZedReady` on every
//! frame; when this module flips the atomic the next poll returns true
//! and the splash exits with its registered animation.
//!
//! Why an atomic instead of an event channel: SplashScreen's poller
//! runs on the Android UI thread (Choreographer-driven), and the
//! "ready" flip happens on the gpui main thread (a different looper
//! on AGDK's GameActivity). A cross-thread atomic read is the smallest
//! possible synchronization that handles the producer/consumer
//! ordering correctly without introducing a queue we'd have to drain.

use std::sync::atomic::{AtomicBool, Ordering};

use jni::{JNIEnv, objects::JClass, sys::jboolean};

static ZED_READY: AtomicBool = AtomicBool::new(false);

/// Called once from the gpui-Rust side after the FIRST successful
/// window paint. Idempotent — subsequent calls are no-ops. The
/// SplashScreen poller short-circuits on the first true reading,
/// removes the splash window, and switches the activity to
/// `Theme.Zdroid.Main`.
pub(crate) fn mark_zed_ready() {
    if !ZED_READY.swap(true, Ordering::AcqRel) {
        log::info!("zed_android::splash: marked ZED_READY=true (first paint completed)");
    }
}

/// JNI entry point for the SplashScreen poller. Returns 1 (true) when
/// the gpui-side has painted at least one frame, 0 (false) otherwise.
///
/// JNI signature: `()Z` — no args, returns jboolean. Kotlin shape is
/// `external fun nativeIsZedReady(): Boolean` on `NativeBridge`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeIsZedReady<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jboolean {
    if ZED_READY.load(Ordering::Acquire) {
        jni::sys::JNI_TRUE
    } else {
        jni::sys::JNI_FALSE
    }
}
