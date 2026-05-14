//! Multi-touch trackpad gesture synthesis.
//!
//! Samsung Book Cover Keyboard's trackpad and presumably other Android
//! trackpads send two-finger scroll as raw multi-pointer `ACTION_MOVE`
//! events on `SOURCE_TOUCHSCREEN`, NOT as `ACTION_SCROLL`. The OS
//! expects the app to recognize the gesture and synthesize the scroll
//! itself. Same shape VNC + X11 hit on Android.
//!
//! State: two separate `Cell`s. One for the primary surface's
//! gesture-in-flight, one for the extra-window surface's. Separate so a
//! gesture in one window doesn't leak its previous frame's centroid into
//! the other.

use std::cell::Cell;

use gpui::{
    Modifiers, MouseButton, MouseUpEvent, PlatformInput, Point, ScrollDelta, ScrollWheelEvent,
    TouchPhase, point, px,
};

thread_local! {
    /// Last centroid (avg of all active pointer positions, in raw pixel
    /// coords pre-scale-divide) of an ongoing multi-touch gesture on the
    /// primary surface. `None` when no multi-touch gesture is in flight.
    static PRIMARY_CENTROID: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// Mirror for the extra-window translator. Separate cell so a
    /// gesture in one window doesn't leak its prev-frame state into the
    /// other.
    static EXTRA_CENTROID: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
}

/// Output of a multi-touch update: an optional `MouseUp(Left)` to cancel
/// an in-flight single-finger press when the gesture transitioned to
/// multi-touch, plus the `ScrollWheelEvent` itself when there's a non-zero
/// delta to report.
pub(crate) struct MultiTouchUpdate {
    pub cancel_left: Option<PlatformInput>,
    pub scroll: Option<PlatformInput>,
}

/// Centroid of all active pointers. Averages out small per-finger jitter
/// and naturally lifts / drops as fingers join or leave the gesture.
pub(crate) fn pointer_centroid(positions: &[(f32, f32, i32)]) -> (f32, f32) {
    let n = positions.len() as f32;
    let sum_x: f32 = positions.iter().map(|(x, _, _)| *x).sum();
    let sum_y: f32 = positions.iter().map(|(_, y, _)| *y).sum();
    (sum_x / n, sum_y / n)
}

/// Compute scroll delta from a centroid update on the primary surface.
/// First frame of a gesture (`prev` is None) just latches state and
/// emits a synthetic `MouseUp(Left)` to cancel any in-flight single-
/// finger press that's now becoming a multi-touch scroll. Subsequent
/// frames emit `ScrollWheelEvent` with the centroid delta.
pub(crate) fn primary_multi_touch_update(
    cur_centroid: (f32, f32),
    position: Point<gpui::Pixels>,
    modifiers: Modifiers,
    scale_factor: f32,
) -> MultiTouchUpdate {
    let prev = PRIMARY_CENTROID.with(|cell| cell.replace(Some(cur_centroid)));
    build_update(prev, cur_centroid, position, modifiers, scale_factor)
}

/// Same as [`primary_multi_touch_update`] but for the extra-window
/// surface. Uses a separate state cell.
pub(crate) fn extra_multi_touch_update(
    cur_centroid: (f32, f32),
    position: Point<gpui::Pixels>,
    modifiers: Modifiers,
    scale_factor: f32,
) -> MultiTouchUpdate {
    let prev = EXTRA_CENTROID.with(|cell| cell.replace(Some(cur_centroid)));
    build_update(prev, cur_centroid, position, modifiers, scale_factor)
}

fn build_update(
    prev: Option<(f32, f32)>,
    cur: (f32, f32),
    position: Point<gpui::Pixels>,
    modifiers: Modifiers,
    scale_factor: f32,
) -> MultiTouchUpdate {
    // First multi-touch frame in the gesture. Cancel any in-flight
    // Left press from the original single-finger Down so gpui doesn't
    // see a stuck button while we emit scrolls. click_count=0 means
    // "not a click".
    let cancel_left = if prev.is_none() {
        Some(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 0,
        }))
    } else {
        None
    };
    let scroll = prev.and_then(|(lx, ly)| {
        let dx = cur.0 - lx;
        let dy = cur.1 - ly;
        if dx == 0.0 && dy == 0.0 {
            return None;
        }
        Some(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(dx / scale_factor), px(dy / scale_factor))),
            modifiers,
            touch_phase: TouchPhase::Moved,
        }))
    });
    MultiTouchUpdate {
        cancel_left,
        scroll,
    }
}

/// End any in-flight primary multi-touch gesture. Called from the
/// dispatcher on `Up`, `PointerUp`, `Cancel`, or when a `Move` arrives
/// with fewer than 2 active pointers (meaning the gesture has resolved
/// back to single-finger).
pub(crate) fn reset_primary() {
    PRIMARY_CENTROID.with(|cell| cell.set(None));
}

/// Mirror of [`reset_primary`] for the extra-window state.
pub(crate) fn reset_extra() {
    EXTRA_CENTROID.with(|cell| cell.set(None));
}
