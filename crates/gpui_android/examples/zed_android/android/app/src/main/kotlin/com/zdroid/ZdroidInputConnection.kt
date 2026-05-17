package com.zdroid

import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection

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
class ZdroidInputConnection(targetView: View) : BaseInputConnection(targetView, /* fullEditor = */ true) {

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        NativeBridge.nativeImeCommitText(text?.toString() ?: "", newCursorPosition)
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        NativeBridge.nativeImeSetComposingText(text?.toString() ?: "", newCursorPosition)
        return true
    }

    override fun finishComposingText(): Boolean {
        NativeBridge.nativeImeFinishComposingText()
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        NativeBridge.nativeImeDeleteSurroundingText(beforeLength, afterLength)
        return true
    }

    override fun sendKeyEvent(event: KeyEvent?): Boolean {
        event ?: return false
        NativeBridge.nativeImeSendKeyEvent(
            event.action,
            event.keyCode,
            event.metaState,
            event.repeatCount,
        )
        return true
    }

    override fun performEditorAction(actionCode: Int): Boolean {
        NativeBridge.nativeImePerformEditorAction(actionCode)
        return true
    }
}
