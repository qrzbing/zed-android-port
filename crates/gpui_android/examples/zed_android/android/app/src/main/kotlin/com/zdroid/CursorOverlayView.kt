package com.zdroid

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.view.View

/// Full-screen transparent overlay that paints a software mouse
/// cursor (bitmap from the bundled apple_cursor pack) in `onDraw` at
/// the tracked (x, y). Hot-spot offsets per style ensure clicks land
/// where the visible sprite points.
///
/// Used by both `MainActivity` and `ExtraWindowActivity` while
/// pointer capture is active; the system PointerIcon is hidden in
/// captured mode so we draw our own sprite on top.
///
/// Style IDs match Android's `PointerIcon.TYPE_*` constants — kept
/// here so both `MainActivity` and `ExtraWindowActivity` reference the
/// same source of truth, and so the Rust side
/// (`crates/gpui_android/src/cursor.rs`) can pass the same integer
/// across the JNI boundary for both the system pointer icon and the
/// captured overlay style.
internal class CursorOverlayView(
    context: Context,
    sizePx: Int,
) : View(context) {
    var cursorX: Float = 0f
    var cursorY: Float = 0f
    var cursorStyle: Int = STYLE_ARROW
    private val sizeInt = sizePx
    private val arrowBitmap = loadCursor(context, R.drawable.cursor_arrow, sizePx)
    private val iBeamBitmap = loadCursor(context, R.drawable.cursor_ibeam, sizePx)
    private val handBitmap = loadCursor(context, R.drawable.cursor_hand, sizePx)
    private val grabBitmap = loadCursor(context, R.drawable.cursor_grab, sizePx)
    private val resizeHBitmap = loadCursor(context, R.drawable.cursor_resize_h, sizePx)
    private val resizeVBitmap = loadCursor(context, R.drawable.cursor_resize_v, sizePx)
    private val hotSpots: Map<Int, Pair<Int, Int>> = mapOf(
        STYLE_ARROW to (sizeInt * 6 / 100 to sizeInt * 6 / 100),
        STYLE_IBEAM to (sizeInt / 2 to sizeInt / 2),
        STYLE_VERTICAL_TEXT to (sizeInt / 2 to sizeInt / 2),
        STYLE_HAND to (sizeInt * 35 / 100 to sizeInt * 6 / 100),
        STYLE_GRAB to (sizeInt / 2 to sizeInt / 2),
        STYLE_GRABBING to (sizeInt / 2 to sizeInt / 2),
        STYLE_HORIZONTAL_DOUBLE_ARROW to (sizeInt / 2 to sizeInt / 2),
        STYLE_VERTICAL_DOUBLE_ARROW to (sizeInt / 2 to sizeInt / 2),
    )
    private val s = sizePx.toFloat()
    private val arrowPath = Path().apply {
        moveTo(0f, 0f)
        lineTo(0f, s * 0.78f)
        lineTo(s * 0.22f, s * 0.60f)
        lineTo(s * 0.38f, s * 0.95f)
        lineTo(s * 0.52f, s * 0.89f)
        lineTo(s * 0.36f, s * 0.55f)
        lineTo(s * 0.62f, s * 0.55f)
        close()
    }
    private val fillPaint = Paint().apply {
        color = Color.WHITE
        style = Paint.Style.FILL
        isAntiAlias = true
    }
    private val strokePaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = 1.5f * context.resources.displayMetrics.density
        strokeJoin = Paint.Join.ROUND
        isAntiAlias = true
    }
    private val bitmapPaint = Paint().apply {
        isAntiAlias = true
        isFilterBitmap = true
    }
    init {
        setWillNotDraw(false)
        isClickable = false
        isFocusable = false
        isFocusableInTouchMode = false
        isHapticFeedbackEnabled = false
        isLongClickable = false
        // Defensive transparency: no background, full opacity on the
        // View itself (the bitmap's own alpha is what makes the
        // cursor visible), and software-layer rendering so the
        // compositor doesn't insert any hardware-accelerated layer
        // alpha behind a paint that should be fully transparent.
        background = null
        alpha = 1f
        setLayerType(LAYER_TYPE_NONE, null)
    }
    fun move(x: Float, y: Float) {
        cursorX = x
        cursorY = y
        invalidate()
    }
    fun setStyle(style: Int) {
        if (cursorStyle != style) {
            cursorStyle = style
            invalidate()
        }
    }
    private fun bitmapForCurrentStyle(): Bitmap? = when (cursorStyle) {
        STYLE_IBEAM, STYLE_VERTICAL_TEXT -> iBeamBitmap
        STYLE_HAND -> handBitmap
        STYLE_GRAB, STYLE_GRABBING -> grabBitmap
        STYLE_HORIZONTAL_DOUBLE_ARROW -> resizeHBitmap
        STYLE_VERTICAL_DOUBLE_ARROW -> resizeVBitmap
        else -> arrowBitmap
    }
    override fun onDraw(canvas: Canvas) {
        val bmp = bitmapForCurrentStyle()
        if (bmp != null) {
            val (hotX, hotY) = hotSpots[cursorStyle] ?: (0 to 0)
            canvas.drawBitmap(
                bmp,
                cursorX - hotX.toFloat(),
                cursorY - hotY.toFloat(),
                bitmapPaint,
            )
            return
        }
        canvas.save()
        canvas.translate(cursorX, cursorY)
        canvas.drawPath(arrowPath, fillPaint)
        canvas.drawPath(arrowPath, strokePaint)
        canvas.restore()
    }

    companion object {
        // PointerIcon.TYPE_* constants — the Rust side (cursor.rs)
        // passes these integers across JNI so both code paths use
        // the same enum values.
        const val STYLE_ARROW = 1000
        const val STYLE_IBEAM = 1008
        const val STYLE_VERTICAL_TEXT = 1009
        const val STYLE_HAND = 1002
        const val STYLE_HORIZONTAL_DOUBLE_ARROW = 1014
        const val STYLE_VERTICAL_DOUBLE_ARROW = 1015
        const val STYLE_GRAB = 1020
        const val STYLE_GRABBING = 1021

        /// Decode a cursor PNG resource into a `Bitmap` scaled to the
        /// target side length. Source bitmaps are ~256px from
        /// apple_cursor's release zip; we downscale at load time so
        /// `onDraw` is a single `drawBitmap` per frame with no
        /// per-frame scaling.
        private fun loadCursor(
            context: Context,
            @androidx.annotation.DrawableRes resId: Int,
            sizePx: Int,
        ): Bitmap {
            val raw = BitmapFactory.decodeResource(context.resources, resId)
            if (raw.width == sizePx && raw.height == sizePx) return raw
            val scaled = Bitmap.createScaledBitmap(raw, sizePx, sizePx, true)
            if (scaled !== raw) raw.recycle()
            return scaled
        }
    }
}

/// Helper that consumes all historical + current samples for a
/// relative MotionEvent axis. Android's `AXIS_RELATIVE_X` /
/// `AXIS_RELATIVE_Y` are NOT accumulated across batched samples;
/// `getAxisValue` returns only the most recent. Without summing the
/// historical samples fast finger motion loses ~80% of its travel
/// (Tab S9 Ultra trackpad batches ~6-10 samples per event).
internal fun sumRelativeAxis(event: android.view.MotionEvent, axis: Int, pointerIndex: Int): Float {
    var sum = 0f
    val historySize = event.historySize
    for (h in 0 until historySize) {
        sum += event.getHistoricalAxisValue(axis, pointerIndex, h)
    }
    sum += event.getAxisValue(axis, pointerIndex)
    return sum
}
