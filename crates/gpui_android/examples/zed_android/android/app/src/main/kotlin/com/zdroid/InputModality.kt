package com.zdroid

import java.util.concurrent.atomic.AtomicBoolean

/// Process-wide single source of truth for which input modality
/// the user was most recently using: hardware pointer (mouse /
/// trackpad / virtual-trackpad-mode touch) or non-pointer
/// (direct touch on the touchscreen, hardware keyboard).
///
/// All Activities in the app (MainActivity + every
/// ExtraWindowActivity) read this flag from `applyCursorVisibility`
/// and write it from their input dispatchers
/// (`handleCapturedEvent` → `set(true)`; `dispatchTouchEvent`,
/// `dispatchKeyEvent` → `set(false)`). Per-Activity flags would
/// drift across window transitions: opening a new
/// ExtraWindowActivity via the trackpad shouldn't reset modality
/// back to "no pointer yet" just because the new window's flag
/// hasn't seen an event. Sharing the state means a fresh window's
/// cursor inherits the app-wide modality immediately on creation.
///
/// `AtomicBoolean` because writes come from input dispatcher
/// callbacks that run on the UI thread, but we don't want to
/// take a chance with future code paths that might write from
/// elsewhere.
object InputModality {
    private val pointerActive = AtomicBoolean(false)

    /// True when the most recent input across the whole process
    /// was a hardware pointer (or virtual-trackpad-mode touch).
    fun isPointer(): Boolean = pointerActive.get()

    /// Mark the current modality as pointer. Called from any
    /// Activity's `handleCapturedEvent` and from
    /// `setTrackpadModeActive(true)`.
    fun setPointer() {
        pointerActive.set(true)
    }

    /// Mark the current modality as non-pointer (direct touch or
    /// keyboard). Called from any Activity's `dispatchTouchEvent`
    /// (when not in trackpad mode) and `dispatchKeyEvent`.
    fun setNonPointer() {
        pointerActive.set(false)
    }
}
