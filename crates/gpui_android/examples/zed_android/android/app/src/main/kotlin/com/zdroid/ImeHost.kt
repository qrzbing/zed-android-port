package com.zdroid

/// Contract every Activity that owns an [ImeHostView] implements,
/// so the host view + its `ZdroidInputConnection` can read state /
/// look up the window id without knowing which Activity class
/// they're attached to. `MainActivity` and `ExtraWindowActivity`
/// both implement this; new Activity types that need a soft IME
/// only have to provide these four hooks.
///
/// All identifiers cross the JNI boundary as `Long` — Rust pairs
/// the `imeWindowId` with each IME event so multi-window dispatch
/// can route to the right gpui-side `PlatformInputHandler`. The
/// primary `MainActivity` uses id `0`; `ExtraWindowActivity` uses
/// the value `gpui` assigned when it opened the window.
interface ImeHost {
    /// Identifier used to route IME events back to the right
    /// gpui window on the Rust side. `0` = primary (MainActivity).
    val imeWindowId: Long

    /// Current [ImeInputMode]. The host view reads this in
    /// `onCreateInputConnection` to set `EditorInfo` flags.
    val currentImeMode: Int

    /// Per-host text state mirror (selection, surrounding text,
    /// composition bounds) the InputConnection's read methods
    /// answer from. Rust pushes updates via
    /// `MainActivity.updateImeTextState` / the extra-activity
    /// equivalent on every commit / compose / delete.
    fun getImeTextState(): ImeTextState?
}
