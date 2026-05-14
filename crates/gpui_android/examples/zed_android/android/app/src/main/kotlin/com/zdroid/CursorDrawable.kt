package com.zdroid

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.Rect
import android.graphics.drawable.Drawable

/// Cursor sprite painted as a SurfaceView FOREGROUND drawable, not
/// as a sibling View in the activity's view hierarchy. The latter
/// triggers Android's compositor to flip the SurfaceView's mode from
/// fully opaque to alpha-aware, which lets gpui's transparent clear
/// color bleed through as a faint whiteish tint across the editor
/// (confirmed on Tab S9 Ultra: removing the sibling CursorOverlayView
/// eliminated the tint entirely).
///
/// `setForeground` is drawn during the View's own draw cycle, on top
/// of its surface buffer, so the SurfaceView remains a sole child of
/// its parent and the compositor keeps it as an opaque layer.
///
/// Style IDs match Android's `PointerIcon.TYPE_*` constants (same as
/// the previous `CursorOverlayView`).
internal class CursorDrawable(
    context: Context,
    private val sizePx: Int,
) : Drawable() {
    /// Cursor position in physical pixels relative to the SurfaceView's
    /// origin. Updated via `move()`.
    private var cursorX: Float = 0f
    private var cursorY: Float = 0f
    var cursorStyle: Int = STYLE_ARROW
        private set
    private val arrowBitmap = loadCursor(context, R.drawable.cursor_arrow, sizePx)
    private val iBeamBitmap = loadCursor(context, R.drawable.cursor_ibeam, sizePx)
    private val handBitmap = loadCursor(context, R.drawable.cursor_hand, sizePx)
    private val grabBitmap = loadCursor(context, R.drawable.cursor_grab, sizePx)
    private val resizeHBitmap = loadCursor(context, R.drawable.cursor_resize_h, sizePx)
    private val resizeVBitmap = loadCursor(context, R.drawable.cursor_resize_v, sizePx)
    private val hotSpots: Map<Int, Pair<Int, Int>> = mapOf(
        STYLE_ARROW to (sizePx * 6 / 100 to sizePx * 6 / 100),
        STYLE_IBEAM to (sizePx / 2 to sizePx / 2),
        STYLE_VERTICAL_TEXT to (sizePx / 2 to sizePx / 2),
        STYLE_HAND to (sizePx * 35 / 100 to sizePx * 6 / 100),
        STYLE_GRAB to (sizePx / 2 to sizePx / 2),
        STYLE_GRABBING to (sizePx / 2 to sizePx / 2),
        STYLE_HORIZONTAL_DOUBLE_ARROW to (sizePx / 2 to sizePx / 2),
        STYLE_VERTICAL_DOUBLE_ARROW to (sizePx / 2 to sizePx / 2),
    )
    private val bitmapPaint = Paint().apply {
        isAntiAlias = true
        isFilterBitmap = true
    }
    private var visible: Boolean = true

    fun move(x: Float, y: Float) {
        if (cursorX != x || cursorY != y) {
            cursorX = x
            cursorY = y
            invalidateSelf()
        }
    }

    fun setStyle(style: Int) {
        if (cursorStyle != style) {
            cursorStyle = style
            invalidateSelf()
        }
    }

    fun setVisible(visible: Boolean) {
        if (this.visible != visible) {
            this.visible = visible
            invalidateSelf()
        }
    }

    private fun bitmapForCurrentStyle(): Bitmap = when (cursorStyle) {
        STYLE_IBEAM, STYLE_VERTICAL_TEXT -> iBeamBitmap
        STYLE_HAND -> handBitmap
        STYLE_GRAB, STYLE_GRABBING -> grabBitmap
        STYLE_HORIZONTAL_DOUBLE_ARROW -> resizeHBitmap
        STYLE_VERTICAL_DOUBLE_ARROW -> resizeVBitmap
        else -> arrowBitmap
    }

    override fun draw(canvas: Canvas) {
        if (!visible) return
        val bmp = bitmapForCurrentStyle()
        val (hotX, hotY) = hotSpots[cursorStyle] ?: (0 to 0)
        canvas.drawBitmap(
            bmp,
            cursorX - hotX.toFloat(),
            cursorY - hotY.toFloat(),
            bitmapPaint,
        )
    }

    override fun setAlpha(alpha: Int) {
        bitmapPaint.alpha = alpha
        invalidateSelf()
    }

    override fun setColorFilter(colorFilter: ColorFilter?) {
        bitmapPaint.colorFilter = colorFilter
        invalidateSelf()
    }

    @Deprecated("Deprecated in Android API 29, but required by Drawable contract")
    override fun getOpacity(): Int = PixelFormat.TRANSLUCENT

    companion object {
        // PointerIcon.TYPE_* constants — match the Rust side (cursor.rs).
        const val STYLE_ARROW = 1000
        const val STYLE_IBEAM = 1008
        const val STYLE_VERTICAL_TEXT = 1009
        const val STYLE_HAND = 1002
        const val STYLE_HORIZONTAL_DOUBLE_ARROW = 1014
        const val STYLE_VERTICAL_DOUBLE_ARROW = 1015
        const val STYLE_GRAB = 1020
        const val STYLE_GRABBING = 1021

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
