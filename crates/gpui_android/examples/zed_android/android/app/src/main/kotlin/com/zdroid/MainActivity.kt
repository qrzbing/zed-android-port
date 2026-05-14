package com.zdroid

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.net.ConnectivityManager
import android.net.Uri
import android.os.Bundle
import android.os.Process
import android.provider.DocumentsContract
import android.util.Log
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity

/// SAF flows go through legacy `startActivityForResult` instead of
/// `ActivityResultLauncher` because `ActivityResultRegistry` silently
/// no-ops `launch()` when the host is in a non-STARTED lifecycle state,
/// which is the typical case when the call comes from a JNI thread driven
/// by gpui's render loop. AGDK's own SAF samples use the legacy path for
/// the same reason — `GameActivity` forwards `onActivityResult` correctly
/// to its Java host, and we get the result without any of the registry
/// gating.
///
/// Multi-window: this Activity hosts only the primary gpui window (the one
/// backing `android_app.native_window()` on the Rust side via GameActivity).
/// Every secondary `cx.open_window` is hosted by a separate
/// [ExtraWindowActivity] launched via Intent, giving each window OS-managed
/// freeform chrome on devices that support it. See `multi_window.rs` and
/// `ExtraWindowActivity.kt`.
class MainActivity : GameActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Edge-to-edge: tell the OS we want to draw behind status / nav bars
        // and the cutout area, so gpui's surface gets the full display
        // bounds. Without this, GameActivity respects system insets and the
        // ANativeWindow we render into is shorter than the screen — visible
        // as letterboxing under the status bar / above the nav bar on
        // 1080x2340 phones (Mi 10) and notch-cropping on tablets.
        //
        // We also hide the system bars by default and set
        // BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE so a downward swipe
        // temporarily reveals the status bar (notifications) without
        // leaving the editor — same UX a native desktop editor gives on
        // Wayland/macOS.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat
                .BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }

        // Pointer-capture probe. When the decor view gains focus we ask
        // Android for raw pointer events. The captured listener
        // stringifies every event and forwards it to Rust for logging
        // only; no synthesis yet. This is here to verify whether Samsung
        // Book Cover Keyboard's trackpad gesture overlay (which
        // collapses two-finger scroll into single-pointer relative
        // motion in non-DeX tablet mode, never firing ACTION_SCROLL)
        // sits above or below the AOSP gesture-recognizer layer that
        // `requestPointerCapture` disables. If captured events show
        // multi-touch with `pointerCount > 1` and proper `AXIS_RELATIVE_*`
        // values, we know we can synthesize scroll on this hardware. If
        // they look identical to the non-captured path, Samsung is
        // intercepting deeper than the AOSP layer and we'd need a
        // different approach.
        //
        // Captured events route to the *focused* View, not decorView.
        // GameActivity sets focus on its SurfaceView, so we install the
        // listener on whatever SurfaceView we find in the hierarchy
        // (decorView's) on top of decorView as a fallback. Setting on
        // both is harmless; whichever the system dispatches to wins.
        // Captured pointer events route through `onGenericMotionEvent`
        // on the Activity (overridden below) — Moonlight's pattern.
        // Avoids the View-level captured-pointer listener path which
        // requires manipulating SurfaceView focus state and on Samsung
        // One UI triggers the accessibility tint + key dispatch
        // regression.
    }

    /// Cursor overlay state. While pointer capture is active the
    /// system cursor is hidden, so we render our own small View on top
    /// of the SurfaceView and update its translation per captured
    /// move event. Position is in physical pixels (the decor view's
    /// coordinate space); the Rust side divides by the surface's
    /// scale factor to get logical pixels for the gpui event.
    private var cursorView: CursorOverlayView? = null
    private var cursorX: Float = 0f
    private var cursorY: Float = 0f
    /// Desktop-classic auto-hide: cursor disappears on the first
    /// keystroke and reappears on any pointer motion. Tracks whether
    /// we're currently in the "hidden by keyboard" state so we don't
    /// thrash visibility on every key.
    private var cursorHiddenByKeyboard: Boolean = false

    /// Called from Rust via JNI (`set_pointer_icon_inner` in
    /// `crates/gpui_android/src/cursor.rs`). Dispatches to the UI
    /// thread because cursorView is a regular Android View and field
    /// writes / invalidation must happen on the UI thread. No-op when
    /// pointer capture is inactive (cursorView is null).
    @Suppress("unused")
    fun setCapturedCursorStyle(style: Int) {
        runOnUiThread {
            cursorView?.setStyle(style)
        }
    }

    override fun onPointerCaptureChanged(hasCapture: Boolean) {
        super.onPointerCaptureChanged(hasCapture)
        Log.i(TAG_CAPTURE, "onPointerCaptureChanged hasCapture=$hasCapture")
        if (hasCapture) {
            ensureCursorView()
            // Center the cursor on capture start so the user has a
            // predictable landing point. Without this the cursor would
            // start at (0, 0) and the first relative motion would
            // travel from the top-left corner.
            val w = window.decorView.width.toFloat()
            val h = window.decorView.height.toFloat()
            cursorX = (w / 2f).coerceAtLeast(0f)
            cursorY = (h / 2f).coerceAtLeast(0f)
            cursorView?.move(cursorX, cursorY)
            cursorView?.visibility = View.VISIBLE
            cursorView?.bringToFront()
        } else {
            cursorView?.visibility = View.GONE
        }
    }

    private fun ensureCursorView() {
        if (cursorView != null) return
        val sizePx = (CURSOR_SIZE_DP * resources.displayMetrics.density).toInt().coerceAtLeast(8)
        val view = CursorOverlayView(this, sizePx)
        // Full-screen so the cursor can be painted anywhere on screen
        // via onDraw, no translation involved (translation on a
        // tiny-bounds View hits Android's compositor clip-to-layout
        // bug and the cursor becomes invisible the moment it leaves
        // its spawn box).
        val lp = ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT,
        )
        val root = window.decorView as ViewGroup
        root.addView(view, lp)
        // Ensure the cursor View ends up topmost in Z-order over the
        // GameActivity SurfaceView. SurfaceView with the default
        // `setZOrderOnTop(false)` composites below the View hierarchy,
        // so bringToFront on the activity-decor side keeps the cursor
        // visible against editor / panel content.
        view.bringToFront()
        cursorView = view
    }

    // installCapturedPointerListenerOnAll removed: we no longer
    // install the View-level captured-pointer listener anywhere.
    // Activity.onGenericMotionEvent below is the single capture path.

    /// Activity-level catch for captured pointer events. Per Moonlight's
    /// pattern (the only Android remote-desktop client that's solved
    /// trackpad input on Samsung tablets): captured events also arrive
    /// here when the window has pointer capture, regardless of which
    /// View has focus. This avoids the `isFocusableInTouchMode=true`
    /// trap that triggers Samsung One UI's accessibility tint and
    /// breaks GameActivity's key dispatch.
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
        // Any pointer activity reawakens the cursor if the keyboard
        // had hidden it.
        if (cursorHiddenByKeyboard) {
            cursorView?.visibility = View.VISIBLE
            cursorHiddenByKeyboard = false
        }
        if (event.actionMasked == MotionEvent.ACTION_MOVE) {
            // Cursor follows the moving finger in three cases:
            //   1. Single-finger motion (n=1): standard cursor drag.
            //   2. Multi-touch while hold-drag is active on the Rust
            //      side (queried via NativeBridge.isHoldDragActive):
            //      the user is selecting text by holding finger 1 and
            //      dragging finger 2, and expects the cursor to follow
            //      the second finger so they can see the selection
            //      growing.
            // For plain two-finger scroll (multi-touch but NOT in
            // hold-drag), cursor stays pinned per desktop standard.
            val isHoldDragMultiTouch = event.pointerCount >= 2 &&
                NativeBridge.isHoldDragActive(PRIMARY_WINDOW_ID)
            if (event.pointerCount == 1 || isHoldDragMultiTouch) {
                var sumRx = 0f
                var sumRy = 0f
                val limit = event.pointerCount
                for (i in 0 until limit) {
                    sumRx += sumRelativeAxis(event, MotionEvent.AXIS_RELATIVE_X, i)
                    sumRy += sumRelativeAxis(event, MotionEvent.AXIS_RELATIVE_Y, i)
                }
                val maxX = window.decorView.width.toFloat().coerceAtLeast(1f)
                val maxY = window.decorView.height.toFloat().coerceAtLeast(1f)
                cursorX = (cursorX + sumRx).coerceIn(0f, maxX - 1f)
                cursorY = (cursorY + sumRy).coerceIn(0f, maxY - 1f)
                cursorView?.move(cursorX, cursorY)
            }
        }
        forwardCapturedPointer(event)
    }

    // `sumRelativeAxis` moved to `CursorOverlayView.kt` as a
    // package-level helper so `ExtraWindowActivity` can use the same
    // implementation. The batching-confirmation probe lives there too.

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        // Desktop-classic auto-hide: hide the cursor on first
        // keystroke. The cursor reappears on any pointer motion via
        // `handleCapturedEvent`. We hide on KEY_DOWN (not UP) so the
        // cursor disappears immediately when the user starts typing,
        // not on the release of a key chord.
        if (event.action == KeyEvent.ACTION_DOWN
            && !cursorHiddenByKeyboard
            && cursorView?.visibility == View.VISIBLE
        ) {
            cursorView?.visibility = View.INVISIBLE
            cursorHiddenByKeyboard = true
        }
        return super.dispatchKeyEvent(event)
    }


    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) {
            if (hasIndirectPointer()) {
                Log.i(TAG_CAPTURE, "requestPointerCapture()")
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
        // `cursorX` / `cursorY` are the canonical cursor position in
        // physical pixels (decorView coordinate space). Kotlin owns
        // this because it also has to position `cursorView` at the
        // same coords; passing it across JNI per event keeps the Rust
        // side from drifting against the visible sprite.
        NativeBridge.nativeOnCapturedPointer(
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

    @Suppress("unused")
    private fun describeCapturedPointer(event: MotionEvent): String {
        val sb = StringBuilder()
        sb.append("act=").append(MotionEvent.actionToString(event.actionMasked))
        sb.append(" src=0x").append(java.lang.Integer.toHexString(event.source))
        sb.append(" btn=0x").append(java.lang.Integer.toHexString(event.buttonState))
        sb.append(" n=").append(event.pointerCount)
        val pc = event.pointerCount.coerceAtMost(3)
        for (i in 0 until pc) {
            sb.append(" p").append(i).append("=(")
            sb.append("%.1f".format(event.getX(i))).append(",")
            sb.append("%.1f".format(event.getY(i))).append(" rx=")
            sb.append("%.2f".format(event.getAxisValue(MotionEvent.AXIS_RELATIVE_X, i))).append(" ry=")
            sb.append("%.2f".format(event.getAxisValue(MotionEvent.AXIS_RELATIVE_Y, i)))
            sb.append(" tt=").append(event.getToolType(i))
            sb.append(")")
        }
        val vs = event.getAxisValue(MotionEvent.AXIS_VSCROLL)
        val hs = event.getAxisValue(MotionEvent.AXIS_HSCROLL)
        if (vs != 0f || hs != 0f) {
            sb.append(" vscroll=").append("%.2f".format(vs))
            sb.append(" hscroll=").append("%.2f".format(hs))
        }
        return sb.toString()
    }

    @Suppress("unused") // called from Rust via JNI
    /**
     * Open an HTTPS URL in the user's default browser. Called via JNI from
     * the Rust side's `cx.open_url(...)`. Empty Android-platform stub before;
     * fix for the runtime picker's "Get module" button which delegates to
     * `cx.open_url(SPAWND_RELEASE_URL)`.
     *
     * Safe to call from any thread; `runOnUiThread` so `startActivity` runs
     * on main. Logs and swallows ActivityNotFoundException (rare on a stock
     * Android with a browser installed; never propagate back to gpui).
     */
    fun openUrl(url: String) {
        Log.i(TAG, "openUrl: $url")
        runOnUiThread {
            try {
                startActivity(
                    Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
                        addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    }
                )
            } catch (t: Throwable) {
                Log.e(TAG, "openUrl: startActivity ACTION_VIEW failed for $url", t)
            }
        }
    }

    fun launchOpenTree() {
        Log.i(TAG, "launchOpenTree() invoked")
        runOnUiThread {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                )
                // Suggest the primary external storage root so the picker
                // lands somewhere familiar instead of "Recent".
                putExtra(
                    DocumentsContract.EXTRA_INITIAL_URI,
                    DocumentsContract.buildRootUri(
                        "com.android.externalstorage.documents",
                        "primary"
                    )
                )
            }
            try {
                startActivityForResult(intent, REQ_OPEN_TREE)
                Log.i(TAG, "startActivityForResult OPEN_DOCUMENT_TREE dispatched")
            } catch (t: Throwable) {
                Log.e(TAG, "OPEN_DOCUMENT_TREE dispatch threw", t)
                onPickerResult("")
            }
        }
    }

    @Suppress("unused") // called from Rust via JNI
    fun launchCreateDocument(suggestedName: String) {
        Log.i(TAG, "launchCreateDocument($suggestedName) invoked")
        runOnUiThread {
            val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "application/octet-stream"
                putExtra(Intent.EXTRA_TITLE, suggestedName)
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                )
            }
            try {
                startActivityForResult(intent, REQ_CREATE_DOCUMENT)
                Log.i(TAG, "startActivityForResult CREATE_DOCUMENT dispatched")
            } catch (t: Throwable) {
                Log.e(TAG, "CREATE_DOCUMENT dispatch threw", t)
                onPickerResult("")
            }
        }
    }

    /// Returns 1 if both READ + WRITE are already granted, 0 if a runtime
    /// dialog has been posted. Caller fires this once on boot and treats
    /// the call as best-effort: if the user denies, file-system reads of
    /// `/storage/emulated/0/...` will EACCES at the syscall layer with a
    /// clean error.
    @Suppress("unused") // called from Rust via JNI
    fun requestStoragePermissions(): Int {
        val needed = listOf(
            Manifest.permission.READ_EXTERNAL_STORAGE,
            Manifest.permission.WRITE_EXTERNAL_STORAGE,
        ).filter {
            ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
        }
        if (needed.isEmpty()) {
            Log.i(TAG, "requestStoragePermissions: already granted")
            return 1
        }
        Log.i(TAG, "requestStoragePermissions: prompting for ${needed.joinToString(",")}")
        runOnUiThread {
            ActivityCompat.requestPermissions(this, needed.toTypedArray(), REQ_STORAGE_PERMS)
        }
        return 0
    }

    /// Returns Android's currently-active DNS server IPs as a comma-joined
    /// string. The Rust side writes them to /sdcard/.zed/r in resolv.conf
    /// format so Bun-compiled CLIs (whose c-ares is patched to read from
    /// /sdcard/.zed/r) can do DNS without proot. Falls back to empty
    /// string if no active network — caller layers in public-DNS defaults.
    @Suppress("unused") // called from Rust via JNI
    fun getActiveDnsServers(): String {
        val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return ""
        val network = cm.activeNetwork ?: return ""
        val props = cm.getLinkProperties(network) ?: return ""
        return props.dnsServers
            .mapNotNull { it.hostAddress }
            .joinToString(",")
    }

    /// Force a clean process exit when the Activity is destroyed.
    ///
    /// gpui_android has multiple static-state init paths (event channels,
    /// JNI globals, OnceLock guards) that assume process-scoped uniqueness.
    /// Android keeps the .so resident across Activity destroy/recreate
    /// cycles when memory pressure or AL_Kill reaps just the Activity but
    /// not the whole process. The next `android_main` re-entry then tries
    /// to re-initialize those statics, which either panics outright
    /// (multi_window event channel: "called twice") or silently leaves the
    /// new gpui state observing stale callbacks bound to the previous
    /// Activity.
    ///
    /// We've declared every config-change axis we care about in
    /// AndroidManifest.xml (`android:configChanges="orientation|...|
    /// uiMode|fontScale|..."`), so rotation, DeX, dark-mode flips, etc.
    /// don't destroy the Activity in the first place — those keep the
    /// process and Activity continuous, no re-entry. The only paths that
    /// reach `onDestroy` are genuine teardowns: user closed the app,
    /// system killed for memory, finishAndRemoveTask. For those, killing
    /// the process here guarantees the next launch starts fresh with
    /// zero stale static state.
    override fun onDestroy() {
        Log.i(TAG, "onDestroy isFinishing=$isFinishing — exiting process for clean restart")
        super.onDestroy()
        Process.killProcess(Process.myPid())
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != REQ_STORAGE_PERMS) {
            return
        }
        val results = permissions.zip(grantResults.toTypedArray()).joinToString(",") { (perm, granted) ->
            "${perm.removePrefix("android.permission.")}=${if (granted == PackageManager.PERMISSION_GRANTED) "OK" else "DENIED"}"
        }
        Log.i(TAG, "onRequestPermissionsResult: $results")
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_OPEN_TREE && requestCode != REQ_CREATE_DOCUMENT) {
            return
        }
        if (resultCode != Activity.RESULT_OK) {
            Log.i(TAG, "picker cancelled (req=$requestCode resultCode=$resultCode)")
            onPickerResult("")
            return
        }
        val uri: Uri? = data?.data
        if (uri != null) {
            try {
                contentResolver.takePersistableUriPermission(
                    uri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                )
            } catch (t: Throwable) {
                Log.w(TAG, "takePersistableUriPermission failed", t)
            }
        }
        onPickerResult(uri?.toString() ?: "")
    }

    private external fun onPickerResult(uriString: String)

    companion object {
        private const val TAG = "zed_android_saf"
        private const val TAG_CAPTURE = "zed_android_capture"
        private const val REQ_OPEN_TREE = 0xA1
        private const val REQ_CREATE_DOCUMENT = 0xA2
        private const val REQ_STORAGE_PERMS = 0xA3
        /// Software cursor side length in dp. 24 dp matches the
        /// classic desktop arrow size at a 1.0x density baseline and
        /// scales naturally with the device's density to feel right
        /// at any DPI without occluding adjacent UI elements.
        private const val CURSOR_SIZE_DP = 24

        // PointerIcon.TYPE_* constants — IDs Rust passes from
        // `cursor.rs` so both code paths use the same enum values.
        /// Matches `captured_pointer::PRIMARY_WINDOW_ID` on the Rust
        /// side. MainActivity always passes this when querying the
        /// per-window hold-drag flag; spawned `ExtraWindowActivity`
        /// instances pass their own `extraWindowId`. The cursor
        /// `STYLE_*` constants live on `CursorOverlayView` so both
        /// Activities share one source of truth.
        const val PRIMARY_WINDOW_ID: Long = 0
    }
}
