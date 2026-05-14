//! Touch gesture state machine.
//!
//! Vocabulary:
//! - **Single-finger tap / drag** → left mouse click / drag (selection
//!   in editor + terminal works as expected)
//! - **Two-finger tap** → right click (the standard tablet / VNC pattern
//!   for invoking `on_secondary_mouse_down` without colliding with text
//!   selection). See `docs/workarounds/two-finger-rightclick.md`.
//! - **Long single-finger press** → just keeps the left button held; no
//!   synthetic right-click. Earlier versions did long-press → right-click;
//!   that interfered with text selection because the selection-and-
//!   context-menu gestures look identical.
//!
//! Trackpad / mouse right-click (BUTTON_SECONDARY in `button_state`) is
//! handled by [`crate::events::mouse`], not here: Android pre-resolves
//! the gesture so we honor `button_state` directly.

use std::cell::Cell;
use std::time::{Duration, Instant};

use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput, Point, Pixels,
};

/// Two-finger tap = right-click. The second finger must come down within
/// this window of the first (and the first must not have drifted into a
/// drag) for the gesture to register as a secondary click. VNC clients
/// and tablet-OS conventions both use the two-finger model rather than
/// long-press because single-finger long-press collides with text
/// selection (which is also a "hold then release" gesture).
const TWO_FINGER_WINDOW: Duration = Duration::from_millis(300);

/// Pixels (logical) the primary finger may drift before we treat the
/// gesture as a drag and stop accepting a 2nd-finger right-click.
const TWO_FINGER_SLOP: f64 = 12.0;

thread_local! {
    static PRIMARY_DOWN: Cell<Option<(Instant, Point<Pixels>)>> = const { Cell::new(None) };
    /// Set when a two-finger tap fired Right-click. The subsequent
    /// `Up` / `PointerUp` events for the gesture should NOT emit Up(Left).
    static RIGHT_CLICK_FIRED: Cell<bool> = const { Cell::new(false) };
}

/// Latch the primary finger position + time. Caller emits the
/// `MouseDown(Left)` separately.
pub(crate) fn record_primary_down(position: Point<Pixels>) {
    PRIMARY_DOWN.with(|cell| cell.set(Some((Instant::now(), position))));
    RIGHT_CLICK_FIRED.with(|cell| cell.set(false));
}

/// Caller (mouse module) tells us they emitted a `MouseDown(Right)` on a
/// trackpad / mouse secondary button so the corresponding Up resolves to
/// `Up(Right)` rather than `Up(Left)`.
pub(crate) fn mark_right_fired() {
    RIGHT_CLICK_FIRED.with(|cell| cell.set(true));
}

/// Returns the synthetic `Cancel(Left) + Down(Right) + Up(Right)`
/// sequence if a second finger landing at `pointer_pos` qualifies as a
/// two-finger tap relative to the latched primary. `None` otherwise.
/// Clears the primary latch when it fires.
pub(crate) fn try_two_finger_right_click(
    pointer_pos: Point<Pixels>,
    modifiers: Modifiers,
) -> Option<Vec<PlatformInput>> {
    let primary_state = PRIMARY_DOWN.with(|cell| cell.get())?;
    let (t, anchor) = primary_state;
    if t.elapsed() >= TWO_FINGER_WINDOW {
        return None;
    }
    if (pointer_pos - anchor).magnitude() > TWO_FINGER_SLOP {
        return None;
    }
    let mut out = Vec::with_capacity(3);
    // Cancel the left-click without firing on_click.
    out.push(PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position: anchor,
        modifiers,
        click_count: 0,
    }));
    // Synthesize a right-click at the primary's spot.
    out.push(PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Right,
        position: anchor,
        modifiers,
        click_count: 1,
        first_mouse: false,
    }));
    out.push(PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Right,
        position: anchor,
        modifiers,
        click_count: 1,
    }));
    RIGHT_CLICK_FIRED.with(|cell| cell.set(true));
    PRIMARY_DOWN.with(|cell| cell.set(None));
    Some(out)
}

/// Outcome of the final `Up` (last pointer lifted or mouse-button released).
pub(crate) enum UpOutcome {
    /// Emit `MouseUp(Left)` to pair with the latched single-finger press.
    EmitLeftUp,
    /// Emit `MouseUp(Right)` to close a trackpad/mouse secondary-button drag.
    /// The two-finger touch path already emitted both Down + Up at
    /// PointerDown time, so this only fires when secondary was held without
    /// a primary touch latch (i.e. mouse / trackpad secondary).
    EmitRightUp,
    /// Nothing to emit (gesture already resolved internally).
    None,
}

pub(crate) fn finalize_up() -> UpOutcome {
    let fired = RIGHT_CLICK_FIRED.with(|cell| cell.take());
    let had_primary = PRIMARY_DOWN.with(|cell| cell.take()).is_some();
    if fired {
        if had_primary {
            UpOutcome::None
        } else {
            UpOutcome::EmitRightUp
        }
    } else if had_primary {
        UpOutcome::EmitLeftUp
    } else {
        UpOutcome::None
    }
}

/// Caller invokes this on every `Move` of a single-pointer drag so the
/// primary latch self-invalidates once the finger drifts past
/// `TWO_FINGER_SLOP`. After invalidation a subsequent second finger no
/// longer qualifies as a two-finger tap (it'd be a "second finger added
/// mid-drag", which we want to treat as a separate gesture, not
/// retroactively a right-click).
pub(crate) fn update_drift(current: Point<Pixels>) {
    PRIMARY_DOWN.with(|cell| {
        if let Some((t, p)) = cell.get() {
            if (current - p).magnitude() > TWO_FINGER_SLOP {
                cell.set(None);
            } else {
                cell.set(Some((t, p)));
            }
        }
    });
}

/// Reset all latches. Called on `Cancel` or when a multi-touch gesture
/// resolves into scroll (so the original press doesn't linger).
pub(crate) fn reset_all() {
    PRIMARY_DOWN.with(|cell| cell.set(None));
    RIGHT_CLICK_FIRED.with(|cell| cell.set(false));
}
