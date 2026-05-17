package com.zdroid

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.drawable.Animatable
import android.net.ConnectivityManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.provider.DocumentsContract
import android.util.Log
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.animation.AccelerateDecelerateInterpolator
import android.widget.FrameLayout
import android.widget.ImageView
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity
import java.io.File

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
    /// Splash overlay shown from `super.onCreate` until the gpui-side
    /// flips `nativeIsZedReady` after first paint. Sits above the
    /// GameActivity `SurfaceView` so the animated Zdroid sigil is
    /// visible the entire time gpui boots, hiding the SurfaceView's
    /// default-black buffer + the wgpu init latency. Removed after
    /// the ready fade.
    private var splashOverlay: FrameLayout? = null
    private val splashHandler = Handler(Looper.getMainLooper())
    private var splashRemoved: Boolean = false

    /// Focusable invisible view that owns the IME `InputConnection`.
    /// Installed in `onCreate`. Rust signals show/hide via JNI calls
    /// to `showIme()` / `hideIme()` on this Activity; those methods
    /// requestFocus on the host and invoke `InputMethodManager`.
    private var imeHostView: ImeHostView? = null

    /// Bring up the soft keyboard. Called from Rust via JNI when an
    /// editor element gains text-input focus (`set_input_handler`).
    /// Idempotent — calling repeatedly when the IME is already up is a
    /// no-op as far as the OS is concerned.
    @Suppress("unused")
    fun showIme() {
        runOnUiThread {
            val host = imeHostView ?: return@runOnUiThread
            host.requestFocus()
            val imm = getSystemService(Context.INPUT_METHOD_SERVICE)
                as android.view.inputmethod.InputMethodManager
            imm.showSoftInput(host, 0)
        }
    }

    /// Dismiss the soft keyboard. Called from Rust when input focus
    /// leaves an editor element (`take_input_handler`).
    @Suppress("unused")
    fun hideIme() {
        runOnUiThread {
            val host = imeHostView ?: return@runOnUiThread
            val imm = getSystemService(Context.INPUT_METHOD_SERVICE)
                as android.view.inputmethod.InputMethodManager
            imm.hideSoftInputFromWindow(host.windowToken, 0)
        }
    }

    private val splashPoll: Runnable = object : Runnable {
        override fun run() {
            if (NativeBridge.nativeIsZedReady()) {
                onZedReady()
                return
            }
            // 32ms = ~30Hz polling. Splash boot waits 2–30s typically,
            // so per-frame polling is overkill; 30Hz keeps the
            // animation smooth and the wake budget low.
            splashHandler.postDelayed(this, 32L)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        installSplashOverlay()
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

        // IME host. Invisible 1x1 view that owns the InputConnection so
        // gpui's text input flows through `ZdroidInputConnection`. Lives
        // alongside GameActivity's SurfaceView; touch dispatch is
        // unaffected (touch goes through the NDK input queue, focus is
        // independent). MainActivity calls `showIme()` / `hideIme()` to
        // bring up / dismiss the keyboard when gpui signals
        // `set_input_handler` / `take_input_handler`.
        val host = ImeHostView(this)
        addContentView(host, android.view.ViewGroup.LayoutParams(1, 1))
        imeHostView = host

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

    /// Cursor position tracked in physical pixels (decorView coordinate
    /// space). Accumulated from each captured-pointer event's
    /// `AXIS_RELATIVE_X`/`AXIS_RELATIVE_Y` deltas; the same value drives
    /// `cursorOverlay.move(...)` (visible sprite, hardware-composited
    /// via SurfaceControl) and is forwarded via JNI as the canonical
    /// cursor position for the gpui-side editor.
    private var cursorX: Float = 0f
    private var cursorY: Float = 0f

    /// Hardware-composited cursor sprite. Lives as a child SurfaceControl
    /// of the GameActivity SurfaceView (API 29+). Null on older devices
    /// and during the brief window between Activity create and
    /// pointer-capture acquire.
    private var cursorOverlay: CursorSurfaceControl? = null

    /// Desktop-classic auto-hide: cursor disappears on the first
    /// keystroke and reappears on any pointer motion. Tracked so we
    /// only toggle visibility on edges, not on every key.
    private var cursorHiddenByKeyboard: Boolean = false

    /// Called from Rust via JNI (`set_pointer_icon_inner` in
    /// `crates/gpui_android/src/cursor.rs`). Dispatches to the UI
    /// thread because the SurfaceControl transaction has to run on a
    /// looper thread. No-op when the overlay isn't live (capture not
    /// active or API < 29).
    @Suppress("unused")
    fun setCapturedCursorStyle(style: Int) {
        runOnUiThread {
            cursorOverlay?.setStyle(style)
        }
    }

    override fun onPointerCaptureChanged(hasCapture: Boolean) {
        super.onPointerCaptureChanged(hasCapture)
        Log.i(TAG_CAPTURE, "onPointerCaptureChanged hasCapture=$hasCapture")
        if (hasCapture) {
            val (w, h) = visibleBounds()
            cursorX = w / 2f
            cursorY = h / 2f
            // Release + rebuild the overlay on every capture-regain so
            // we anchor to the *current* SurfaceView's SurfaceControl.
            // The OS can tear down and recreate the SurfaceView's
            // surface when another activity steals focus (SAF picker
            // when the user clicks "Open Project" from onboarding,
            // settings dialogs, etc.), and any SurfaceControl we
            // previously attached as a child of the old surface gets
            // orphaned by SurfaceFlinger — `setVisible(true)` on the
            // orphan does nothing and the cursor stays invisible
            // until the app fully restarts. Rebuilding on regain is
            // cheap (small bitmap upload + one SurfaceControl
            // transaction) and bulletproof.
            cursorOverlay?.release()
            cursorOverlay = null
            ensureCursorOverlay()
            cursorOverlay?.move(cursorX, cursorY)
            cursorOverlay?.setVisible(true)
        } else {
            cursorOverlay?.setVisible(false)
        }
    }

    /// Build the SurfaceControl overlay on first capture-gain. API 29+
    /// gated; older Android leaves the field null and the trackpad
    /// continues to work without a visible cursor sprite.
    private fun ensureCursorOverlay() {
        if (cursorOverlay != null) return
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        val surfaceView = findSurfaceView(window.decorView) ?: return
        val displaySize = (CURSOR_SIZE_DP * resources.displayMetrics.density)
            .toInt()
            .coerceAtLeast(16)
        cursorOverlay = CursorSurfaceControl(this, surfaceView, displaySize)
    }

    private fun visibleBounds(): Pair<Float, Float> {
        val sv = findSurfaceView(window.decorView)
        val w = (sv?.width ?: window.decorView.width).toFloat().coerceAtLeast(1f)
        val h = (sv?.height ?: window.decorView.height).toFloat().coerceAtLeast(1f)
        return w to h
    }

    private fun findSurfaceView(view: View): SurfaceView? {
        if (view is SurfaceView) return view
        if (view is ViewGroup) {
            for (i in 0 until view.childCount) {
                val found = findSurfaceView(view.getChildAt(i))
                if (found != null) return found
            }
        }
        return null
    }

    /// Attach an animated splash overlay above the GameActivity
    /// SurfaceView. Stays visible until the gpui-Rust side flips
    /// `nativeIsZedReady` (first paint completed), at which point
    /// `onZedReady` fades the overlay out and removes it. The
    /// overlay covers the SurfaceView's default-black buffer + the
    /// wgpu boot latency, so the user sees a continuous animation
    /// from cold start through to the editor's first frame instead
    /// of icon → black → editor.
    ///
    /// Why a sibling View overlay rather than a separate
    /// SplashActivity:
    ///   - A separate activity must finish before MainActivity is
    ///     visible, but gpui (and SurfaceView) can only init while
    ///     MainActivity is visible, so the transition unavoidably
    ///     drops the animation mid-boot.
    ///   - The View-overlay path normally triggers SurfaceView's
    ///     compositor flip to alpha-aware mode (cursor white-tint
    ///     regression). Sidestepped here because the wgpu surface
    ///     is already configured with `transparent: true` +
    ///     `set_clear_color` to opaque brand indigo; the wgpu
    ///     output is always fully opaque once it draws anything,
    ///     so alpha-aware compositing has nothing transparent to
    ///     bleed through.
    private fun installSplashOverlay() {
        if (splashOverlay != null) return
        val container = FrameLayout(this).apply {
            layoutParams = ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            )
            setBackgroundResource(R.color.zdroid_bg)
        }
        val displaySizePx = (200 * resources.displayMetrics.density).toInt()
        val iconParams = FrameLayout.LayoutParams(displaySizePx, displaySizePx).apply {
            gravity = android.view.Gravity.CENTER
        }
        val iconView = ImageView(this).apply {
            layoutParams = iconParams
            setImageResource(R.drawable.splash_icon_animated)
            contentDescription = null
        }
        container.addView(iconView)
        val decor = window.decorView as? ViewGroup
        decor?.addView(container)
        splashOverlay = container
        (iconView.drawable as? Animatable)?.start()
        splashHandler.post(splashPoll)
    }

    private fun onZedReady() {
        if (splashRemoved) return
        splashRemoved = true
        splashHandler.removeCallbacks(splashPoll)
        val overlay = splashOverlay ?: return
        // Fade alpha + scale up ~10% (the "ripple dissipates" exit
        // that echoes the launcher icon's brand motif). 350ms feels
        // intentional without stalling the user's first input.
        overlay.animate()
            .alpha(0f)
            .scaleX(1.10f)
            .scaleY(1.10f)
            .setDuration(350L)
            .setInterpolator(AccelerateDecelerateInterpolator())
            .withEndAction {
                (overlay.parent as? ViewGroup)?.removeView(overlay)
                splashOverlay = null
            }
            .start()
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
        // hid it.
        if (cursorHiddenByKeyboard) {
            cursorOverlay?.setVisible(true)
            cursorHiddenByKeyboard = false
        }
        if (event.actionMasked == MotionEvent.ACTION_MOVE) {
            // Cursor follows the moving finger in two cases:
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
                val (maxX, maxY) = visibleBounds()
                cursorX = (cursorX + sumRx).coerceIn(0f, maxX - 1f)
                cursorY = (cursorY + sumRy).coerceIn(0f, maxY - 1f)
                cursorOverlay?.move(cursorX, cursorY)
            }
        }
        forwardCapturedPointer(event)
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (event.action == KeyEvent.ACTION_DOWN
            && !cursorHiddenByKeyboard
            && cursorOverlay != null
        ) {
            cursorOverlay?.setVisible(false)
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

    /// Returns the running app's versionName (e.g. "0.2.0"). The in-app
    /// updater compares this against GitHub's `releases/latest` tag
    /// (e.g. "v0.2.1" with the `v` stripped) to decide whether to
    /// download an upgrade.
    @Suppress("unused") // called from Rust via JNI
    fun appVersionName(): String {
        return try {
            packageManager.getPackageInfo(packageName, 0).versionName ?: ""
        } catch (t: Throwable) {
            Log.w(TAG_UPDATE, "appVersionName: PackageManager threw", t)
            ""
        }
    }

    /// Hand a downloaded APK to Android's package installer. Rust
    /// calls this after the updater finishes writing the APK to
    /// `cacheDir/updater/zdroid-<tag>.apk`. We wrap the path in a
    /// FileProvider content:// URI (per the manifest provider
    /// declaration at `.updater.fileprovider`) so the installer can
    /// read across the app-private boundary; FLAG_GRANT_READ_URI_PERMISSION
    /// is what makes that grant explicit.
    ///
    /// Returns true on a successful intent dispatch (the installer UI
    /// will then take over and prompt the user). Returns false if the
    /// file is missing or the installer can't be started — Rust logs
    /// the failure but doesn't retry.
    @Suppress("unused") // called from Rust via JNI
    fun launchPackageInstaller(apkPath: String): Boolean {
        val file = File(apkPath)
        if (!file.exists()) {
            Log.e(TAG_UPDATE, "launchPackageInstaller: APK missing at $apkPath")
            return false
        }
        return try {
            val uri = FileProvider.getUriForFile(
                this,
                "com.zdroid.updater.fileprovider",
                file,
            )
            val intent = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            startActivity(intent)
            true
        } catch (t: Throwable) {
            Log.e(TAG_UPDATE, "launchPackageInstaller dispatch failed", t)
            false
        }
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
        splashHandler.removeCallbacksAndMessages(null)
        cursorOverlay?.release()
        cursorOverlay = null
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
        private const val TAG_UPDATE = "zed_android_update"
        private const val REQ_OPEN_TREE = 0xA1
        private const val REQ_CREATE_DOCUMENT = 0xA2
        private const val REQ_STORAGE_PERMS = 0xA3
        /// Software cursor side length in dp. Scaled by display
        /// density at instantiation time to give the sprite a
        /// consistent visual size across devices.
        private const val CURSOR_SIZE_DP = 24
        /// Matches `captured_pointer::PRIMARY_WINDOW_ID` on the Rust
        /// side. MainActivity always passes this when querying the
        /// per-window hold-drag flag; spawned `ExtraWindowActivity`
        /// instances pass their own `extraWindowId`.
        const val PRIMARY_WINDOW_ID: Long = 0
    }
}
