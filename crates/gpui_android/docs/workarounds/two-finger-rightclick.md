# Two-finger tap → right click

**Status:** Active
**Phase / Commit:** `3193130a4b` — Switch Android right-click to two-finger tap
(supersedes earlier `d4998d1d19` long-press-as-right-click)
**Files:** `crates/gpui_android/src/window.rs`, `crates/gpui_android/src/events.rs`

## Problem

Touchscreens have no right mouse button. The first attempt was long-press
→ right click, but it interfered with text selection in the terminal (and
any element where the user wanted to select text first and then invoke a
context menu): the synthesized `Down(Left)` cleared the prior selection
before the synthesized `Down(Right)` could show the menu, so the user got
the menu but their selection was gone.

## Constraint

We need a touchscreen gesture that:

- Doesn't interfere with text selection (so not long-press)
- Doesn't interfere with normal tap (so not single-tap)
- Maps cleanly to Android's `MotionEvent` model so we can detect at
  `Down`/`Up` boundaries
- Coexists with USB mouse / trackpad right-click (which arrives via
  `MotionEvent`'s `button_state` with `BUTTON_SECONDARY` set)

VNC / tablet-OS convention: **two-finger tap = right-click on the primary
finger's position.**

## Solution

Detection at `MotionAction::Down` for touch sources:

- Single finger: emit `MouseDown(Left)` as normal.
- Second finger arrives within **300 ms** of the first **and** inside a
  **12 px slop window** of the first touch: cancel the buffered `Down(Left)`,
  emit `Down(Right)` + `Up(Right)` synthetically, set `RIGHT_CLICK_FIRED`
  latch.
- Outside the timing window or slop = treat as a separate gesture
  (resize, two-finger scroll). Don't fire right-click.

For trackpad / USB mouse right-click: Android resolves the gesture for us
and sends `MotionEvent` with `BUTTON_SECONDARY` in `button_state`. Skip the
touch-style detection, emit `Down(Right)`/`Up(Right)` directly.

PointerUp is now a no-op — the two-finger gesture resolves entirely at
`PointerDown` time. `RIGHT_CLICK_FIRED` suppresses the duplicate `Up(Left)`
the regular Up handler would otherwise emit.

## Why this works

- Single-finger interactions (selection, drag, tap) are unchanged — no
  cancellation, no synthesis.
- Two-finger tap is a deliberate user gesture that wouldn't otherwise be
  used for single-touch operations.
- The 300 ms / 12 px slop catches "two fingers landing nearly
  simultaneously on the same target" while rejecting "second finger added
  to existing drag" (which would be far apart in time or space).
- Trackpad / mouse path is handled by Android's gesture resolver — we
  just respect what `button_state` says.

## Failure mode if regressed

- Long-press reintroduced as right-click → text selection broken in
  terminal and elsewhere (the old bug).
- Slop too tight → two genuine two-finger taps slightly offset get
  rejected as separate single-tap gestures.
- Slop too loose → two-finger scroll gets misclassified as right-click.
- `RIGHT_CLICK_FIRED` latch missing → duplicate `Up(Left)` after
  right-click fires; UI may double-handle (e.g. close menu just opened).

## See also

- [first-mouse-tagging.md](first-mouse-tagging.md)
- [jvm-clipboard-stack-overflow.md](jvm-clipboard-stack-overflow.md)
