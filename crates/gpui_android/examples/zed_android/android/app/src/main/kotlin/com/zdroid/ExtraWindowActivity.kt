package com.zdroid

import android.content.Context
import android.os.Bundle
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

/// Host for a single secondary gpui window. Spawned by Rust via JNI
/// `startActivity(Intent(...))` from `multi_window::launch_extra_activity`,
/// with the gpui `WindowId` passed as the `com.zdroid.window_id`
/// long extra. On freeform-windowing devices (DeX, Pixel desktop windowing,
/// Android 16 Desktop Mode, ChromeOS) the OS provides native chrome —
/// close X, drag bar, resize handles. On phones each Activity lives in its
/// own Recents task instead.
///
/// Lifecycle:
/// - `onCreate` registers this Activity instance with Rust via
///   `NativeBridge.nativeOnExtraActivityCreated`. Rust stores a `GlobalRef`
///   keyed by the window id so it can later issue `finishAndRemoveTask` for
///   gpui-initiated close.
/// - `SurfaceHolder.Callback` fires through `NativeBridge.nativeOnExtraSurface*`
///   — same JNI bridge as the primary surface, just keyed by window id.
/// - `OnTouchListener` forwards `MotionEvent`s through
///   `NativeBridge.nativeOnExtraTouchEvent`.
/// - `onDestroy` notifies Rust via `nativeOnExtraActivityDestroyed`. If Rust
///   triggered the destruction (`finishAndRemoveTask` from the gpui side),
///   the registry entry is already gone and the notify is idempotent. If the
///   user clicked the OS chrome X, this is the path that drives gpui-side
///   `Window::remove_window()` via the registered `on_close` callback.
///
/// Native lib loading: handled by `ZedApplication.onCreate` before any
/// Activity instantiates. Do NOT add a per-Activity `companion object init`
/// block here.
///
/// Activity recreation: `configChanges` in the manifest is exhaustive
/// enough to keep this Activity alive across drag-resize, rotation, density
/// change, locale change, etc. — the system delivers `onConfigurationChanged`
/// instead of recreating. If a config we forgot to declare ever fires
/// recreation, `onDestroy` notifies Rust which tears down the gpui Window;
/// the user's window disappears, which is bad UX. Test by aggressive
/// drag-resize after any manifest change.
class ExtraWindowActivity : AppCompatActivity() {
    private var extraWindowId: Long = -1L
    private lateinit var surfaceView: SurfaceView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Edge-to-edge to match MainActivity. On phone (non-freeform) this
        // makes the secondary surface fill the screen end-to-end. On
        // freeform-windowing devices the OS-managed chrome (close X, drag
        // bar) renders on its own decoration layer above this Activity, so
        // hiding system bars here doesn't strip the chrome — only the
        // status / nav strips that don't belong to the freeform window.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat
                .BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }

        extraWindowId = intent.getLongExtra(EXTRA_WINDOW_ID, -1L)
        if (extraWindowId < 0L) {
            Log.e(TAG, "onCreate: missing or invalid $EXTRA_WINDOW_ID extra; finishing")
            finish()
            return
        }
        Log.i(TAG, "onCreate windowId=$extraWindowId")

        // Process-death recovery: if Android killed our process and brought
        // this Activity back from Recents, the gpui-side runtime has been
        // re-init'd from scratch and doesn't know about our windowId. Running
        // through the rest of onCreate would attach a SurfaceView that fires
        // JNI callbacks against a Rust runtime with no matching gpui Window —
        // touches do nothing, no rendering, ghost window. Detect early and
        // bail. The user can re-open via the main app.
        if (!NativeBridge.nativeIsExtraWindowKnown(extraWindowId)) {
            Log.w(TAG, "onCreate windowId=$extraWindowId not known to Rust runtime (resurrection?); finishing")
            finish()
            return
        }

        NativeBridge.nativeOnExtraActivityCreated(extraWindowId, this)

        val id = extraWindowId
        surfaceView = ScrollableSurfaceView(this).apply {
            holder.setFormat(android.graphics.PixelFormat.RGBA_8888)
            holder.addCallback(SurfaceCallback(id))
            // Forward touches to native via JNI. Returning true claims the
            // gesture so it doesn't bubble up to the OS chrome (which would
            // try to re-route to the drag handle).
            setOnTouchListener { _, event ->
                forwardTouchEvent(id, event)
                true
            }
            // ACTION_HOVER_ENTER / ACTION_HOVER_MOVE / ACTION_HOVER_EXIT
            // (mouse moving over the surface without a button pressed) come
            // through OnHoverListener — NOT OnTouchListener. Without this,
            // gpui never sees a `MouseMove { pressed_button: None }`, which
            // is what scrollbar-on-hover, link previews, and any
            // hover-only UI affordance is gated on.
            setOnHoverListener { _, event ->
                forwardTouchEvent(id, event)
                true
            }
            // ACTION_SCROLL (mouse wheel + trackpad two-finger scroll) and
            // mouse button events on SOURCE_MOUSE come through
            // OnGenericMotionListener. Without this, scrolling with a
            // hardware mouse / trackpad over a settings/secondary window
            // is a no-op.
            setOnGenericMotionListener { _, event ->
                forwardTouchEvent(id, event)
                true
            }
            // SurfaceView wants to be the focusable target for IME / key
            // events; AppCompatActivity's default content view doesn't
            // grant focus on its own.
            isFocusable = true
            isFocusableInTouchMode = true
        }
        setContentView(surfaceView)
    }

    override fun onDestroy() {
        Log.i(TAG, "onDestroy windowId=$extraWindowId")
        if (extraWindowId >= 0L) {
            NativeBridge.nativeOnExtraActivityDestroyed(extraWindowId)
        }
        super.onDestroy()
    }

    private inner class SurfaceCallback(private val id: Long) : SurfaceHolder.Callback {
        override fun surfaceCreated(holder: SurfaceHolder) {
            Log.i(TAG, "surfaceCreated windowId=$id")
            NativeBridge.nativeOnExtraSurfaceCreated(id, holder.surface)
        }

        override fun surfaceChanged(
            holder: SurfaceHolder,
            format: Int,
            width: Int,
            height: Int,
        ) {
            Log.i(TAG, "surfaceChanged windowId=$id ${width}x$height fmt=$format")
            NativeBridge.nativeOnExtraSurfaceChanged(id, holder.surface, format, width, height)
        }

        override fun surfaceDestroyed(holder: SurfaceHolder) {
            Log.i(TAG, "surfaceDestroyed windowId=$id")
            NativeBridge.nativeOnExtraSurfaceDestroyed(id)
        }
    }

    private fun forwardTouchEvent(id: Long, event: MotionEvent) {
        val pointerCount = event.pointerCount
        if (pointerCount <= 0) return
        val xs = FloatArray(pointerCount)
        val ys = FloatArray(pointerCount)
        val ids = IntArray(pointerCount)
        for (i in 0 until pointerCount) {
            xs[i] = event.getX(i)
            ys[i] = event.getY(i)
            ids[i] = event.getPointerId(i)
        }
        // ACTION_SCROLL (mouse wheel + trackpad two-finger scroll) carries
        // its delta on the AXIS_VSCROLL / AXIS_HSCROLL axes — getX/Y return
        // the pointer position, not the scroll amount. Read both axes
        // unconditionally; they're zero on non-scroll events and the Rust
        // translator only consumes them under the Scroll action arm.
        val vscroll = event.getAxisValue(MotionEvent.AXIS_VSCROLL)
        val hscroll = event.getAxisValue(MotionEvent.AXIS_HSCROLL)
        NativeBridge.nativeOnExtraTouchEvent(
            id,
            event.actionMasked,
            event.actionIndex,
            event.metaState,
            event.buttonState,
            event.eventTime,
            vscroll,
            hscroll,
            xs,
            ys,
            ids,
        )
    }

    companion object {
        private const val TAG = "zed_android_extra"
        const val EXTRA_WINDOW_ID = "com.zdroid.window_id"
    }
}

/// `SurfaceView` subclass that advertises itself as scrollable to the
/// Android input subsystem. Android's input dispatcher decides whether
/// to synthesize an `ACTION_SCROLL` event from a trackpad two-finger
/// gesture by walking up from the pointer's hit-test target and asking
/// each `View` whether it `canScrollVertically()` / `canScrollHorizontally()`.
/// A bare `SurfaceView` returns `false` for both — the OS then falls
/// back to delivering the gesture as a single-pointer fake-mouse drag
/// (Down with `button_state=0` then Move ×N then Up), which gpui then
/// (correctly, given the input it sees) interprets as a click+drag.
///
/// Returning `true` for both axes flips Android's behavior: trackpad
/// two-finger swipes get translated to `ACTION_SCROLL` with VSCROLL /
/// HSCROLL axes set, which the existing primary + extra translators
/// already consume. Mouse wheel still delivers `ACTION_SCROLL` directly
/// (independent of this override).
///
/// Why this is on a custom subclass and not on the SurfaceView fields
/// directly: `View.canScrollVertically` is a `protected open fun` —
/// can only be overridden, not set, so we need a class.
private class ScrollableSurfaceView(context: Context) : SurfaceView(context) {
    override fun canScrollVertically(direction: Int): Boolean = true
    override fun canScrollHorizontally(direction: Int): Boolean = true
}
