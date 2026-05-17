package com.zdroid

import android.content.Context
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection

/// Focusable invisible view that hosts the IME `InputConnection`.
/// GameActivity's `SurfaceView` renders the editor and receives touch
/// input via the NDK native input queue, but it doesn't (and can't
/// easily be subclassed to) provide an `InputConnection` of our
/// choosing. This view sits alongside the `SurfaceView` in the
/// Activity's content hierarchy at 1×1 px (effectively invisible) and
/// is the focus target whenever the editor wants the soft keyboard.
///
/// Focus is requested explicitly from `MainActivity` when Rust signals
/// "this window now has text input focus" (via `set_input_handler`).
/// Touch events stay on the `SurfaceView`; keyboard focus comes here
/// without affecting touch dispatch (the two are independent in
/// Android's input model).
class ImeHostView(context: Context) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // Multi-line text input (editor) with no fullscreen / extract
        // UI (we render the editor ourselves; the IME shouldn't
        // overlay a fullscreen text view).
        outAttrs.inputType =
            EditorInfo.TYPE_CLASS_TEXT or EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE
        outAttrs.imeOptions =
            EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI
        return ZdroidInputConnection(this)
    }
}
