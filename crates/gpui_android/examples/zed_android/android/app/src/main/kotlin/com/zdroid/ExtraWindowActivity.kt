package com.zdroid

import android.content.Context
import android.os.Bundle
import android.util.Log
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
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
    /// Software cursor overlay + capture-mode state. Mirrors
    /// MainActivity's pipeline so the trackpad behaves identically
    /// inside spawned windows: requestPointerCapture intercepts raw
    /// touchpad events, onGenericMotionEvent routes them to the
    /// per-window Rust state machine via
    /// `NativeBridge.nativeOnExtraCapturedPointer(extraWindowId, …)`,
    /// and the overlay paints the bitmap cursor on top of the
    /// SurfaceView. `cursorX`/`cursorY` are physical pixels in
    /// decorView coordinate space; Kotlin owns the visible cursor
    /// position so the on-screen sprite and the gpui-side cursor
    /// stay synchronized.
    private var cursorView: CursorOverlayView? = null
    private var cursorX: Float = 0f
    private var cursorY: Float = 0f
    /// Container for `surfaceView` + `cursorView`. Both share the
    /// same coordinate origin (this FrameLayout's top-left). Adding
    /// the cursorView to decorView instead means the sprite paints
    /// in decorView coords while the editor receives MouseDown in
    /// surface coords; in non-edge-to-edge / freeform-windowing
    /// layouts the two diverge, producing the "cursor on top panel,
    /// click hits debugger below" symptom and capping cursor travel
    /// at the smaller of the two widths.
    private lateinit var contentRoot: FrameLayout
    private var cursorHiddenByKeyboard: Boolean = false

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
            //
            // CRITICAL: when pointer capture is active for a connected
            // trackpad / mouse, the captured-pointer pipeline (the
            // Activity-level `onGenericMotionEvent` override below)
            // owns those events. If this listener returns `true` for
            // captured events it claims them and the Activity handler
            // never fires — the cursor sprite never updates and the
            // editor gets raw absolute-coordinate hovers instead of
            // synthesized MouseMove events from the gesture state
            // machine. Return `false` for captured sources so the
            // event falls through to the Activity handler.
            setOnGenericMotionListener { _, event ->
                val source = event.source
                val isCaptureSource =
                    source and InputDevice.SOURCE_TOUCHPAD != 0
                        || source and InputDevice.SOURCE_MOUSE_RELATIVE != 0
                        || source and InputDevice.SOURCE_MOUSE != 0
                if (isCaptureSource && window.decorView.hasPointerCapture()) {
                    false
                } else {
                    forwardTouchEvent(id, event)
                    true
                }
            }
            // SurfaceView wants to be the focusable target for IME / key
            // events; AppCompatActivity's default content view doesn't
            // grant focus on its own.
            isFocusable = true
            isFocusableInTouchMode = true
            // Captured-pointer events arrive via the Activity-level
            // `onGenericMotionEvent` override below — same pattern as
            // MainActivity. We don't install an
            // `OnCapturedPointerListener` here because some Samsung
            // builds bypass that listener path when DeX windowing
            // is active.
        }
        // Wrap surfaceView in a FrameLayout so the cursor overlay can
        // be added as a sibling with a shared coordinate origin.
        contentRoot = FrameLayout(this).apply {
            addView(
                surfaceView,
                FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.MATCH_PARENT,
                    FrameLayout.LayoutParams.MATCH_PARENT,
                ),
            )
        }
        setContentView(contentRoot)
    }

    override fun onDestroy() {
        Log.i(TAG, "onDestroy windowId=$extraWindowId")
        if (extraWindowId >= 0L) {
            NativeBridge.nativeOnExtraActivityDestroyed(extraWindowId)
        }
        super.onDestroy()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // Mirror MainActivity: request capture when this window gains
        // focus and a trackpad/mouse is connected, release when focus
        // is lost. Without this, spawned windows fall back to
        // Samsung's gesture filter which mangles trackpad gestures
        // into single-finger fake-mouse events.
        if (hasFocus) {
            if (hasIndirectPointer()) {
                Log.i(TAG, "requestPointerCapture() windowId=$extraWindowId")
                window.decorView.requestPointerCapture()
            }
        } else {
            window.decorView.releasePointerCapture()
        }
    }

    private fun hasIndirectPointer(): Boolean {
        val ids = InputDevice.getDeviceIds()
        for (id in ids) {
            val dev = InputDevice.getDevice(id) ?: continue
            val sources = dev.sources
            if (sources and InputDevice.SOURCE_TOUCHPAD != 0) return true
            if (sources and InputDevice.SOURCE_MOUSE != 0) return true
            if (sources and InputDevice.SOURCE_MOUSE_RELATIVE != 0) return true
        }
        return false
    }

    override fun onPointerCaptureChanged(hasCapture: Boolean) {
        super.onPointerCaptureChanged(hasCapture)
        Log.i(TAG, "onPointerCaptureChanged windowId=$extraWindowId hasCapture=$hasCapture")
        if (hasCapture) {
            ensureCursorView()
            // One-shot diagnostic so we can see where any size
            // mismatch comes from when "cursor escapes the right
            // edge" is reported on the device.
            Log.i(
                TAG,
                "dimensions windowId=$extraWindowId " +
                "surface=${surfaceView.width}x${surfaceView.height} " +
                "cursorView=${cursorView?.width}x${cursorView?.height} " +
                "contentRoot=${contentRoot.width}x${contentRoot.height} " +
                "decor=${window.decorView.width}x${window.decorView.height}",
            )
            val (w, h) = visibleBounds()
            cursorX = w / 2f
            cursorY = h / 2f
            cursorView?.move(cursorX, cursorY)
            cursorView?.visibility = View.VISIBLE
            cursorView?.bringToFront()
        } else {
            cursorView?.visibility = View.GONE
        }
    }

    /// Most defensive bounds for cursor clamping: the smallest of
    /// surfaceView / cursorView / contentRoot widths and heights.
    /// If any of these report dimensions larger than the actual
    /// visible content area (e.g., the system reports surface
    /// dimensions in raw display pixels while the activity is
    /// rendered in a smaller freeform window), the smallest one
    /// represents the truly drawable region.
    private fun visibleBounds(): Pair<Float, Float> {
        val cv = cursorView
        val w = listOfNotNull(surfaceView.width, cv?.width, contentRoot.width)
            .filter { it > 0 }
            .minOrNull() ?: 1
        val h = listOfNotNull(surfaceView.height, cv?.height, contentRoot.height)
            .filter { it > 0 }
            .minOrNull() ?: 1
        return w.toFloat() to h.toFloat()
    }

    private fun ensureCursorView() {
        if (cursorView != null) return
        val sizePx = (CURSOR_SIZE_DP * resources.displayMetrics.density).toInt().coerceAtLeast(8)
        val view = CursorOverlayView(this, sizePx)
        val initW = surfaceView.width.takeIf { it > 0 }
            ?: FrameLayout.LayoutParams.MATCH_PARENT
        val initH = surfaceView.height.takeIf { it > 0 }
            ?: FrameLayout.LayoutParams.MATCH_PARENT
        contentRoot.addView(view, FrameLayout.LayoutParams(initW, initH))
        view.bringToFront()
        cursorView = view
        // Sync cursorView bounds + position to surfaceView whenever
        // it re-layouts. Without this the view's size stays at
        // whatever it was the first time we created it — including
        // across orientation changes where the activity stays alive
        // via configChanges but the surface dimensions flip. Symptom:
        // cursor sprite paints past the visible screen on right /
        // bottom edges because the cursorView's bounds were left
        // larger than the new visible surface.
        surfaceView.addOnLayoutChangeListener { _, left, top, right, bottom, _, _, _, _ ->
            val w = right - left
            val h = bottom - top
            if (w > 0 && h > 0) {
                val params = view.layoutParams
                params.width = w
                params.height = h
                view.layoutParams = params
                view.x = left.toFloat()
                view.y = top.toFloat()
            }
        }
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        val source = event.source
        val isMouseRel = source and InputDevice.SOURCE_MOUSE_RELATIVE != 0
        val isTouchpad = source and InputDevice.SOURCE_TOUCHPAD != 0
        val isMouse = source and InputDevice.SOURCE_MOUSE != 0
        if ((isMouseRel || isTouchpad || isMouse)
            && window.decorView.hasPointerCapture()) {
            handleCapturedEvent(event)
            return true
        }
        return super.onGenericMotionEvent(event)
    }

    private fun handleCapturedEvent(event: MotionEvent) {
        if (cursorHiddenByKeyboard) {
            cursorView?.visibility = View.VISIBLE
            cursorHiddenByKeyboard = false
        }
        if (event.actionMasked == MotionEvent.ACTION_MOVE) {
            val isHoldDragMultiTouch = event.pointerCount >= 2 &&
                NativeBridge.isHoldDragActive(extraWindowId)
            if (event.pointerCount == 1 || isHoldDragMultiTouch) {
                var sumRx = 0f
                var sumRy = 0f
                val limit = event.pointerCount
                for (i in 0 until limit) {
                    sumRx += sumRelativeAxis(event, MotionEvent.AXIS_RELATIVE_X, i)
                    sumRy += sumRelativeAxis(event, MotionEvent.AXIS_RELATIVE_Y, i)
                }
                val (maxX, maxY) = visibleBounds()
                cursorX = (cursorX + sumRx).coerceIn(0f, maxX - 1f)
                cursorY = (cursorY + sumRy).coerceIn(0f, maxY - 1f)
                cursorView?.move(cursorX, cursorY)
            }
        }
        forwardCapturedPointer(event)
    }

    private fun forwardCapturedPointer(event: MotionEvent) {
        val n = event.pointerCount
        val xs = FloatArray(n)
        val ys = FloatArray(n)
        val rxs = FloatArray(n)
        val rys = FloatArray(n)
        for (i in 0 until n) {
            xs[i] = event.getX(i)
            ys[i] = event.getY(i)
            rxs[i] = sumRelativeAxis(event, MotionEvent.AXIS_RELATIVE_X, i)
            rys[i] = sumRelativeAxis(event, MotionEvent.AXIS_RELATIVE_Y, i)
        }
        val vs = event.getAxisValue(MotionEvent.AXIS_VSCROLL)
        val hs = event.getAxisValue(MotionEvent.AXIS_HSCROLL)
        NativeBridge.nativeOnExtraCapturedPointer(
            extraWindowId,
            event.actionMasked,
            event.source,
            event.buttonState,
            n,
            xs,
            ys,
            rxs,
            rys,
            vs,
            hs,
            cursorX,
            cursorY,
        )
    }

    /// Called from Rust via JNI (`cursor.rs::set_pointer_icon_inner`)
    /// to update the cursor sprite shape inside this window. Same
    /// shape as MainActivity's setCapturedCursorStyle.
    @Suppress("unused")
    fun setCapturedCursorStyle(style: Int) {
        runOnUiThread {
            cursorView?.setStyle(style)
        }
    }

    /// Forward hardware key events to the gpui-side window. AppCompatActivity
    /// doesn't have GameActivity's native input queue, so without this
    /// override the OS routes KeyEvents to the focused View's default
    /// handler (which is a no-op for our SurfaceView) and gpui's
    /// `PlatformInput::KeyDown` never fires for editors in extra windows.
    /// Observed regression: Settings search bar focus worked (Editor's
    /// `Focused` event fired, `set_input_handler` registered), but typing
    /// did nothing because no KeyDown ever arrived.
    ///
    /// We forward every event up-front, then return `true` to claim it so
    /// Android's fallback IME routing doesn't try to steal keystrokes
    /// from an editor that thinks it has focus. ACTION_MULTIPLE events
    /// (synthesized soft-keyboard character sequences) are forwarded
    /// too; the Rust side drops them since gpui has no PlatformInput
    /// mapping for that action, which is the same policy as the primary
    /// window's translate_key_event uses.
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        // Desktop-classic auto-hide: hide the cursor on first
        // keystroke. Reappears on any pointer motion via
        // `handleCapturedEvent`. Mirrors MainActivity behavior.
        if (event.action == KeyEvent.ACTION_DOWN
            && !cursorHiddenByKeyboard
            && cursorView?.visibility == View.VISIBLE
        ) {
            cursorView?.visibility = View.INVISIBLE
            cursorHiddenByKeyboard = true
        }
        if (extraWindowId >= 0L) {
            NativeBridge.nativeOnExtraKeyEvent(
                extraWindowId,
                event.action,
                event.keyCode,
                event.metaState,
                event.repeatCount,
            )
            return true
        }
        return super.dispatchKeyEvent(event)
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
        /// Cursor size matches MainActivity's so the sprite is
        /// identical across all windows the user can spawn.
        private const val CURSOR_SIZE_DP = 24
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
