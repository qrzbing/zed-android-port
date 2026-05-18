package com.zdroid

import android.util.Log
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.ExtractedText
import android.view.inputmethod.ExtractedTextRequest

private const val TAG = "zdroid_ime"

/// Bridge from Android's IME (Gboard, Swiftkey, etc.) into gpui's
/// `PlatformInputHandler`. The IME calls methods here for every text
/// edit it wants to perform; we forward each into Rust via
/// `NativeBridge`, and Rust dispatches into the focused window's
/// `PlatformInputHandler` (the same trait macOS NSTextInputClient
/// and Linux text-input-v3 wire to).
///
/// Composition (CJK / prediction / gesture typing) uses
/// `setComposingText` / `finishComposingText`. Plain commits (final
/// keystrokes, voice input) use `commitText`. Backspace / delete uses
/// `deleteSurroundingText`. Hardware-style key events that the IME
/// wants to deliver (Enter, arrows) flow through `sendKeyEvent` and
/// land on the existing `events/keyboard.rs` translator.
///
/// Returns `true` from each method to signal the IME we consumed the
/// event. Returning `false` would cause the IME to fall back to
/// posting the text as KeyEvents, which we explicitly don't want
/// (loses composition info).
class ZdroidInputConnection(private val hostView: View) : BaseInputConnection(hostView, /* fullEditor = */ true) {

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val s = text?.toString() ?: ""
        Log.i(TAG, "IC.commitText text=${quote(s)} cursor=$newCursorPosition")
        NativeBridge.nativeImeCommitText(s, newCursorPosition)
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val s = text?.toString() ?: ""
        Log.i(TAG, "IC.setComposingText text=${quote(s)} cursor=$newCursorPosition")
        NativeBridge.nativeImeSetComposingText(s, newCursorPosition)
        return true
    }

    override fun finishComposingText(): Boolean {
        Log.i(TAG, "IC.finishComposingText")
        NativeBridge.nativeImeFinishComposingText()
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        Log.i(TAG, "IC.deleteSurroundingText before=$beforeLength after=$afterLength")
        NativeBridge.nativeImeDeleteSurroundingText(beforeLength, afterLength)
        return true
    }

    override fun sendKeyEvent(event: KeyEvent?): Boolean {
        event ?: return false
        Log.i(
            TAG,
            "IC.sendKeyEvent action=${event.action} keyCode=${event.keyCode} " +
                "meta=0x${Integer.toHexString(event.metaState)} repeat=${event.repeatCount} " +
                "unicode=${event.unicodeChar} chars=${quote(event.characters ?: "")}"
        )
        NativeBridge.nativeImeSendKeyEvent(
            event.action,
            event.keyCode,
            event.metaState,
            event.repeatCount,
        )
        return true
    }

    override fun performEditorAction(actionCode: Int): Boolean {
        Log.i(TAG, "IC.performEditorAction action=$actionCode")
        NativeBridge.nativeImePerformEditorAction(actionCode)
        return true
    }

    // ---- Read path ----
    // The IME queries these to know the current text + selection
    // state so it can refine predictions, position candidates, etc.
    // Without these, Gboard's state-sync model can't trust its
    // internal view, and it re-sends commits / compositions
    // defensively (the ~155ms duplicate setComposingText pattern we
    // saw in the log). We answer from MainActivity's
    // `ImeTextState` mirror, which Rust pushes via JNI on every text
    // change.

    private fun mirror(): ImeTextState =
        (hostView.context as? MainActivity)?.getImeTextState() ?: ImeTextState.EMPTY

    override fun getTextBeforeCursor(n: Int, flags: Int): CharSequence? {
        val text = mirror().textBeforeCursor(n)
        Log.i(TAG, "IC.getTextBeforeCursor n=$n -> len=${text.length}")
        return text
    }

    override fun getTextAfterCursor(n: Int, flags: Int): CharSequence? {
        val text = mirror().textAfterCursor(n)
        Log.i(TAG, "IC.getTextAfterCursor n=$n -> len=${text.length}")
        return text
    }

    override fun getSelectedText(flags: Int): CharSequence? {
        val text = mirror().selectedText()
        if (text.isEmpty()) return null
        Log.i(TAG, "IC.getSelectedText -> len=${text.length}")
        return text
    }

    override fun getExtractedText(request: ExtractedTextRequest?, flags: Int): ExtractedText? {
        val state = mirror()
        Log.i(
            TAG,
            "IC.getExtractedText -> len=${state.text.length} sel=${state.selectionStart}..${state.selectionEnd}"
        )
        return state.extractedText()
    }

    private fun quote(s: String): String =
        "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
}
