//! Mouse input translation. Buttons, wheel, hover, drag.
//!
//! Android pre-resolves trackpad / mouse multi-finger gestures and
//! physical buttons into `MotionEvent.button_state`. A two-finger tap on
//! the Galaxy Book Cover trackpad arrives here as a single pointer with
//! `BUTTON_SECONDARY` set; physical right-click on a USB mouse arrives the
//! same way. We honor `button_state` directly so the user doesn't have
//! to do anything special with a trackpad.

use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, TouchPhase, point, px,
};

/// `MotionEvent.BUTTON_SECONDARY` — set when the user clicks the right
/// mouse button or does a two-finger tap on a touchpad.
pub(crate) const ANDROID_BUTTON_SECONDARY: i32 = 1 << 1;

/// Build the `MouseDown(Right)` produced when Android reports
/// BUTTON_SECONDARY at the start of a gesture (trackpad two-finger tap,
/// mouse right-click). Caller must also flag the touch state machine via
/// [`crate::events::touch::mark_right_fired`] so the matching Up is
/// emitted as Right not Left.
pub(crate) fn secondary_button_down(
    position: Point<Pixels>,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Right,
        position,
        modifiers,
        click_count: 1,
        first_mouse: false,
    })
}

/// Build the `MouseDown(Left)` for a single-finger touch tap / mouse
/// left-click. `first_mouse: false` because there's no window-focus
/// concept on Android; setting `true` would make every click look like
/// a focus-the-window-first click, which `ClickEvent::first_focus`
/// returns as true. Listeners like ProjectPanel's on_click bail on a
/// "first focus" click, so files would never open / folders would never
/// expand.
pub(crate) fn primary_button_down(
    position: Point<Pixels>,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count: 1,
        first_mouse: false,
    })
}

/// Build the `MouseUp` paired with a previously-emitted Down. Caller
/// decides the button (typically driven by
/// [`crate::events::touch::finalize_up`]).
pub(crate) fn button_up(
    button: MouseButton,
    position: Point<Pixels>,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::MouseUp(MouseUpEvent {
        button,
        position,
        modifiers,
        click_count: 1,
    })
}

/// Build the `ScrollWheelEvent` for an `ACTION_SCROLL` event (mouse
/// wheel, mouse-equivalent trackpad scroll). Android reports +Y for
/// "up" (away from user) and the historical translator negated it
/// before handing to gpui. Preserved here for the scaffolding refactor;
/// the inversion is removed in a follow-up commit.
pub(crate) fn wheel_scroll(
    vscroll: f32,
    hscroll: f32,
    position: Point<Pixels>,
    modifiers: Modifiers,
) -> Option<PlatformInput> {
    if vscroll == 0.0 && hscroll == 0.0 {
        return None;
    }
    Some(PlatformInput::ScrollWheel(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(hscroll, -vscroll)),
        modifiers,
        touch_phase: TouchPhase::Moved,
    }))
}

/// Pixel-precision scroll synthesized from multi-touch centroid delta.
/// Trackpad two-finger swipes that arrive as multi-pointer ACTION_MOVE
/// (Samsung Book Cover style) feed into this. Caller does the scale-
/// factor divide and supplies logical-pixel deltas.
pub(crate) fn pixel_scroll(
    delta: Point<Pixels>,
    position: Point<Pixels>,
    modifiers: Modifiers,
) -> PlatformInput {
    PlatformInput::ScrollWheel(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(delta),
        modifiers,
        touch_phase: TouchPhase::Moved,
    })
}
