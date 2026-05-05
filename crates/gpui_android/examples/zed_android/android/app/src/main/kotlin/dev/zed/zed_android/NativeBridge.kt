package dev.zed.zed_android

import android.view.Surface

/// Single source of truth for all JNI declarations bridging Kotlin → Rust
/// for multi-window surface lifecycle and touch dispatch. Both
/// `MainActivity` and `ExtraWindowActivity` call into this object so the
/// JNI symbol set lives in one place — adding a new bridge fn means a single
/// declaration here and a single matching `Java_..._NativeBridge_*` extern in
/// `crates/gpui_android/src/multi_window.rs`.
///
/// **JNI symbol mangling:** these `external fun`s resolve to
/// `Java_dev_zed_zed_1android_NativeBridge_<methodName>` symbols. Class name
/// changes here require matching renames on the Rust side.
object NativeBridge {
    /// Process-death recovery probe. `ExtraWindowActivity.onCreate` calls
    /// this BEFORE any other JNI work. Returns true if the gpui-side has a
    /// live AndroidWindow registered for this `windowId` (this Activity was
    /// launched in the current Rust process and gpui knows about it).
    /// Returns false on resurrection from Recents after a process kill —
    /// gpui has been re-init'd and has no record of the windowId. Activity
    /// uses the result to either proceed or `finish()` itself.
    external fun nativeIsExtraWindowKnown(windowId: Long): Boolean

    /// Posted from `ExtraWindowActivity.onCreate`. Rust stores a `GlobalRef`
    /// to the activity in its registry so it can later issue
    /// `finishAndRemoveTask` for gpui-initiated close. Must fire BEFORE
    /// `nativeOnExtraSurfaceCreated` (the SurfaceHolder.Callback may not
    /// have run yet at this point — that's fine, ordering is enforced by
    /// the Activity lifecycle).
    external fun nativeOnExtraActivityCreated(windowId: Long, activity: Any)

    /// Posted from `ExtraWindowActivity.onDestroy`. Removes the GlobalRef
    /// from the registry and posts an `OsClosed` event so the gpui-side
    /// `Window::remove_window()` flow runs.
    external fun nativeOnExtraActivityDestroyed(windowId: Long)

    /// Posted by a `SurfaceHolder.Callback` when the surface is first ready.
    /// Rust unwraps the Surface into an `ANativeWindow` and either resolves
    /// the pending `oneshot` (first attach) or routes through the event
    /// channel (re-attach after Activity recreation).
    external fun nativeOnExtraSurfaceCreated(windowId: Long, surface: Surface)

    /// Posted by `surfaceChanged`. Width/height drive the Vulkan swapchain
    /// reconfigure on the Rust side.
    external fun nativeOnExtraSurfaceChanged(
        windowId: Long,
        surface: Surface,
        format: Int,
        width: Int,
        height: Int,
    )

    /// Posted by `surfaceDestroyed` — Rust must stop submitting frames
    /// synchronously inside this callback.
    external fun nativeOnExtraSurfaceDestroyed(windowId: Long)

    /// MotionEvent fields marshaled into primitive arrays (we can't share
    /// `MotionEvent` across the JNI boundary). Pointer indices are
    /// `0..pointerCount-1`.
    external fun nativeOnExtraTouchEvent(
        windowId: Long,
        actionMasked: Int,
        actionIndex: Int,
        metaState: Int,
        buttonState: Int,
        eventTimeMillis: Long,
        xs: FloatArray,
        ys: FloatArray,
        pointerIds: IntArray,
    )
}
