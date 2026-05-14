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

use crate::events::source::{InputSource, multi_click_slop, multi_click_window};

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

// `MULTI_CLICK_WINDOW` and `MULTI_CLICK_SLOP` are no longer constants;
// they're per-source via `events::source::multi_click_window` /
// `multi_click_slop`. Mouse / stylus get tighter timing + slop to match
// `ViewConfiguration.getDoubleTapTimeout()` (~300ms / ~3px) which is
// the system convention for indirect pointing. Finger taps keep the
// historical 500ms / 6px.

thread_local! {
    static PRIMARY_DOWN: Cell<Option<(Instant, Point<Pixels>)>> = const { Cell::new(None) };
    /// Set when a non-primary mouse / trackpad button is currently
    /// being held (or, for the two-finger touch path, was momentarily
    /// held and resolved in-place at PointerDown time). The
    /// corresponding `Up` should emit `MouseUp(stored_button)` instead
    /// of the default `Up(Left)`.
    static HELD_NON_PRIMARY: Cell<Option<MouseButton>> = const { Cell::new(None) };
    /// Last Down event's timestamp + position + button + run-length.
    /// A new Down within `MULTI_CLICK_WINDOW` + `MULTI_CLICK_SLOP` of
    /// the previous one of the same button bumps the run length, which
    /// becomes the `click_count` on the new `MouseDownEvent`. Word /
    /// line-select in the editor key off this.
    static LAST_CLICK: Cell<Option<(Instant, Point<Pixels>, MouseButton, usize)>> =
        const { Cell::new(None) };
}

/// Latch the primary finger position + time. Caller emits the
/// `MouseDown(Left)` separately.
pub(crate) fn record_primary_down(position: Point<Pixels>) {
    PRIMARY_DOWN.with(|cell| cell.set(Some((Instant::now(), position))));
    HELD_NON_PRIMARY.with(|cell| cell.set(None));
}

/// Caller (mouse module) tells us they emitted a `MouseDown(button)` on
/// a trackpad / mouse non-primary button so the corresponding Up
/// resolves to `Up(button)` rather than `Up(Left)`.
pub(crate) fn mark_non_primary_down(button: MouseButton) {
    HELD_NON_PRIMARY.with(|cell| cell.set(Some(button)));
}

/// Returns the currently-held non-primary button (if any). Used by the
/// Move handler to populate `MouseMoveEvent::pressed_button` for
/// non-primary drag.
pub(crate) fn current_non_primary() -> Option<MouseButton> {
    HELD_NON_PRIMARY.with(|cell| cell.get())
}

/// Compute the `click_count` for a new Down at `position` with
/// `button` on `source`. If the previous Down was the same button,
/// recent (within `multi_click_window(source)`), and nearby (within
/// `multi_click_slop(source)`), the run-length bumps; otherwise it
/// resets to 1. Caller stamps the returned count onto the emitted
/// `MouseDownEvent` so the editor's word- / line-select on double- /
/// triple-click works.
///
/// `source` is plumbed by the dispatcher so we apply the system's
/// 300ms / 3px hardware-pointer timing when the click came from a
/// mouse / stylus / trackpad, and keep the historical 500ms / 6px for
/// finger taps.
pub(crate) fn next_click_count(
    button: MouseButton,
    position: Point<Pixels>,
    source: InputSource,
) -> usize {
    let now = Instant::now();
    let window = multi_click_window(source);
    let slop = multi_click_slop(source);
    let count = LAST_CLICK.with(|cell| match cell.get() {
        Some((t, p, b, c))
            if b == button
                && now.duration_since(t) < window
                && (position - p).magnitude() <= slop =>
        {
            c + 1
        }
        _ => 1,
    });
    LAST_CLICK.with(|cell| cell.set(Some((now, position, button, count))));
    if count > 1 {
        log::info!(
            "multi_click: source={source:?} count={count} window={window:?} \
             slop={slop} button={button:?}"
        );
    }
    count
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
    // Gesture fully resolved in-place: no need to latch HELD_NON_PRIMARY,
    // and we clear PRIMARY_DOWN so the final ACTION_UP doesn't try to
    // emit another Up(Left).
    HELD_NON_PRIMARY.with(|cell| cell.set(None));
    PRIMARY_DOWN.with(|cell| cell.set(None));
    Some(out)
}

/// Outcome of the final `Up` (last pointer lifted or mouse-button released).
pub(crate) enum UpOutcome {
    /// Emit `MouseUp(button)` to close the gesture. `button` is `Left`
    /// for the touch / primary-finger path and the corresponding
    /// non-primary button for trackpad / mouse secondary, middle, or
    /// navigate-back/forward drags.
    Emit(MouseButton),
    /// Nothing to emit (gesture already resolved internally — e.g. the
    /// touch two-finger right-click resolved at PointerDown time).
    None,
}

pub(crate) fn finalize_up() -> UpOutcome {
    let held = HELD_NON_PRIMARY.with(|cell| cell.take());
    let had_primary = PRIMARY_DOWN.with(|cell| cell.take()).is_some();
    if let Some(button) = held {
        UpOutcome::Emit(button)
    } else if had_primary {
        UpOutcome::Emit(MouseButton::Left)
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
    HELD_NON_PRIMARY.with(|cell| cell.set(None));
}
