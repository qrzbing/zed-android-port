# Extra-window input parity (hover, scroll, secondary)

**Status:** Active
**Phase / Commit:** L9 follow-up (input UX polish)
**Files:**
- `crates/gpui_android/examples/zed_android/android/app/src/main/kotlin/dev/zed/zed_android/ExtraWindowActivity.kt`
- `crates/gpui_android/examples/zed_android/android/app/src/main/kotlin/dev/zed/zed_android/NativeBridge.kt`
- `crates/gpui_android/src/events.rs` (`translate_extra_motion_event`)
- `crates/gpui_android/src/multi_window.rs` (`Java_..._nativeOnExtraTouchEvent` + `ExtraWindowEvent::Motion`)
- `crates/gpui_android/src/platform.rs` (Motion drain arm)

## Symptom

In secondary gpui windows hosted by `ExtraWindowActivity` (Settings,
Keymap, Themes, Extensions, Image / Markdown / SVG previews when they
get popped out, etc.), three categories of mouse / trackpad input
silently no-op while the same input works in the primary `MainActivity`
window:

- **Hover-to-show scrollbar** — moving the mouse pointer (no button
  pressed) into a scrollable region doesn't fade the thumb in. The
  scrollbar only appears once the user clicks somewhere, after which
  the scrollbar autohide state machine treats the click as a hover and
  shows it for the standard 1500ms.
- **Mouse wheel + trackpad two-finger scroll** — both produce no scroll
  on the secondary window. Same gesture works in the editor / terminal
  / project panel inside MainActivity.
- **Right-click via mouse / trackpad** — no context menu, no
  `on_secondary_mouse_down` handlers fire.

## Root cause

Two-layer gap:

1. **`ExtraWindowActivity.kt`'s `SurfaceView` only installed
   `setOnTouchListener`.** Hover events (`ACTION_HOVER_*`) come through
   `setOnHoverListener`; scroll-wheel + button events come through
   `setOnGenericMotionListener`. Neither was wired, so the system never
   delivered them to our forwarder.

2. **`translate_extra_motion_event` was deliberately a stub** — only
   handled `ACTION_DOWN`, `ACTION_UP`, `ACTION_MOVE`. The dropped cases
   are documented in the source comment: "Intentionally simpler than the
   primary translator… secondary gpui windows aren't yet wired."
   The primary translator at `events.rs:83` already handles hover,
   scroll, and `BUTTON_SECONDARY`, so the editor / terminal / project
   panel — all hosted by MainActivity / GameActivity — work fine.

The extra-window event pipeline (Java MotionEvent → JNI marshaling →
`ExtraWindowEvent::Motion` → drain on game thread →
`translate_extra_motion_event` → `state.handle_input(...)`) is otherwise
identical to the primary path; the only delta is that the extra path
re-builds `MotionEvent`-shaped data from raw fields because we can't
share a Java `MotionEvent` reference across the JNI boundary.

## Fix

### 1. Listeners on `ExtraWindowActivity.kt`'s `SurfaceView`

```kotlin
setOnTouchListener { _, event -> forwardTouchEvent(id, event); true }
setOnHoverListener { _, event -> forwardTouchEvent(id, event); true }
setOnGenericMotionListener { _, event -> forwardTouchEvent(id, event); true }
```

All three forward to the same `forwardTouchEvent` JNI bridge — the Rust
side dispatches by `event.actionMasked` so a single bridge serves all
sources. Returning `true` from each listener claims the event so the OS
chrome (drag handle, freeform resize edges) doesn't try to re-route it.

### 2. Bridge two new floats through the JNI signature

`ACTION_SCROLL` carries its delta in `event.getAxisValue(AXIS_VSCROLL)`
/ `AXIS_HSCROLL` — `getX/Y` return the pointer position, NOT the scroll
amount. The Kotlin side reads both axes unconditionally; they're zero
on non-scroll events. Rust-side `Java_..._nativeOnExtraTouchEvent` and
`ExtraWindowEvent::Motion` carry them through.

### 3. Mirror primary-translator action arms in `translate_extra_motion_event`

Added `JAVA_ACTION_HOVER_MOVE` (7) → `MouseMove { pressed_button: None }`
and `JAVA_ACTION_SCROLL` (8) → `ScrollWheelEvent { delta: Lines(point(hscroll, -vscroll)) }`.
Honored `button_state & BUTTON_SECONDARY` on Down/Move/Up so
mouse / trackpad right-click maps to `MouseButton::Right`.

What's NOT mirrored: two-finger-tap → right-click synthesis (the
PointerDown branch at `events.rs:142-180`). Settings / Keymap / Themes
don't surface a context menu, so the gesture would have nowhere to go.
Add when we ship a secondary window that actually needs it.

## Why we don't (yet) merge the two translators

The primary translator consumes `android_activity::input::MotionEvent`
(NDK-backed, gives access to private MotionAction constructor + axis
helpers). The extra translator consumes raw fields marshaled across
JNI from a Java `MotionEvent` (the Kotlin side reads + arrays them up
because we can't share a `jobject` across the C++/Rust boundary
safely on the gpui main thread). They could be unified behind a
shared trait if we end up adding more input-vocabulary cases —
right-click synthesis, drag-to-scroll, fling momentum — but for now
~30 lines of duplication beat the abstraction cost.

## Failure mode if regressed

- New listener removed from `ExtraWindowActivity.kt` → hover / scroll /
  buttons silently no-op in extra windows again. Editor / terminal in
  MainActivity stay working (different translator), masking the bug.
- `event.getAxisValue(AXIS_VSCROLL)` not extracted on the Kotlin side
  → mouse wheel returns zero delta, scroll fires but moves nothing.
  Subtle: action arm runs, no panic, just a no-op.
- New extern fn signature in `nativeOnExtraTouchEvent` not matched on
  the Kotlin `external fun nativeOnExtraTouchEvent(...)` side → JNI
  signature mismatch crash on first touch event in any extra window.
  `UnsatisfiedLinkError` at the call site, not at load time.

## See also

- [activate-extra-activity-move-to-front.md](activate-extra-activity-move-to-front.md)
  — how secondary windows get focused for `cx.windows()` dedup
- [touch-input-polish.md](touch-input-polish.md) — primary-translator
  scroll wheel handling we mirrored here (if that doc exists; otherwise
  this is the first writeup of the equivalent work for extras)
