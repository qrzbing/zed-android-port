package com.zdroid

/// IME input target classifications. Keep IN SYNC with Rust's
/// `ime::ImeTargetKind::to_jni_int` — the int values cross the JNI
/// boundary into [MainActivity.restartImeForTarget].
///
/// Termux's TerminalView uses two analogous modes (line 280 of
/// TerminalView.java for char-stream, line 287 for raw-key TYPE_NULL).
/// We collapse them into a single TERMINAL mode using the char-stream
/// flags (VISIBLE_PASSWORD + NO_SUGGESTIONS) because Gboard still
/// needs to deliver text — TYPE_NULL on Gboard refuses to compose AT
/// ALL which would break punctuation, autoshift, etc.
object ImeInputMode {
    const val TERMINAL: Int = 0
    const val CODE_EDITOR: Int = 1
}
