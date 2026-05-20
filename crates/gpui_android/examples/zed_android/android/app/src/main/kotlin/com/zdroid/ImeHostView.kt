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

    /// Returning `true` here unconditionally is what makes Android
    /// auto-show the IME the moment this view gains focus — same
    /// behavior as EditText (Android special-cases "text editor"
    /// views in `InputMethodManager.startInputInner`). When a
    /// hardware keyboard is connected via USB or Bluetooth,
    /// `Configuration.keyboard` flips to `KEYBOARD_QWERTY` /
    /// `KEYBOARD_12KEY`. Detecting that and returning `false`
    /// suppresses the auto-show — Termux's TerminalView is a plain
    /// `View` that doesn't override this method at all, which is
    /// why Termux never shows Gboard with a HW keyboard attached
    /// even though the user's IME setting is unchanged.
    ///
    /// `onCreateInputConnection` is still callable when we manually
    /// `imm.showSoftInput(host)`, so explicit shows from
    /// `MainActivity.showIme` (gated on the `on_screen_keyboard`
    /// setting) still work in the touch-only case.
    override fun onCheckIsTextEditor(): Boolean {
        val hwKeyboardPresent = resources.configuration.keyboard !=
            android.content.res.Configuration.KEYBOARD_NOKEYS
        val isTextEditor = !hwKeyboardPresent
        android.util.Log.i(
            "zdroid_ime",
            "ImeHostView.onCheckIsTextEditor() -> $isTextEditor (hwKeyboard=$hwKeyboardPresent)",
        )
        return isTextEditor
    }

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        val mode = (context as? ImeHost)?.currentImeMode ?: ImeInputMode.CODE_EDITOR
        android.util.Log.i("zdroid_ime", "ImeHostView.onCreateInputConnection mode=$mode")
        // EditorInfo per target kind (see ImeInputMode + ImeTargetKind):
        //
        // - TERMINAL: Termux's TerminalView.java line 280 pattern.
        //   TYPE_TEXT_VARIATION_VISIBLE_PASSWORD disables composition
        //   AND prediction in Gboard / Swiftkey. Means each keystroke
        //   commits directly to the PTY (no setComposingText
        //   accumulation, no autocorrect dump-and-retry).
        //
        // - CODE_EDITOR: NO_SUGGESTIONS kills the autocorrect strip
        //   over code tokens, but we deliberately DON'T set
        //   VISIBLE_PASSWORD — that would also disable CJK
        //   composition, which we want to keep working for users
        //   writing comments / strings in their native script.
        //   IME_MULTI_LINE so Enter inserts a newline instead of
        //   triggering "Done".
        // Both modes use VISIBLE_PASSWORD: Samsung's Gboard ignores
        // NO_SUGGESTIONS alone (documented behavior — its prediction
        // strip + glide-typing still fire on code tokens, producing
        // the "lililimeline" autocompletion regression where Gboard
        // turns a sequence of "lili" typings into a long suggested
        // word and commits the whole thing). VISIBLE_PASSWORD is the
        // only flag that reliably refuses composition + prediction
        // across Gboard / Swiftkey / Samsung IME. Trade-off: CJK
        // composition is also disabled in CODE_EDITOR — we add a
        // separate RICH_TEXT mode later if a user needs CJK input
        // (rare in code; common in comments / docs).
        outAttrs.inputType = when (mode) {
            ImeInputMode.TERMINAL ->
                EditorInfo.TYPE_CLASS_TEXT or
                    EditorInfo.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
                    EditorInfo.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            else /* CODE_EDITOR */ ->
                EditorInfo.TYPE_CLASS_TEXT or
                    EditorInfo.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
                    EditorInfo.TYPE_TEXT_FLAG_NO_SUGGESTIONS or
                    EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE
        }
        outAttrs.imeOptions =
            EditorInfo.IME_FLAG_NO_FULLSCREEN or
                EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
        return ZdroidInputConnection(this)
    }

    override fun onFocusChanged(
        gainFocus: Boolean,
        direction: Int,
        previouslyFocusedRect: android.graphics.Rect?,
    ) {
        super.onFocusChanged(gainFocus, direction, previouslyFocusedRect)
        android.util.Log.i("zdroid_ime", "ImeHostView.onFocusChanged gain=$gainFocus")
    }
}
