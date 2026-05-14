//! Android input → gpui `PlatformInput` translation.
//!
//! Two entry points:
//! - [`translate_motion_event`] for the primary surface, which receives
//!   NDK `MotionEvent`s through android-activity's native input queue.
//! - [`translate_extra_motion_event`] for extra-window surfaces, which
//!   receive raw `MotionEvent` fields marshaled across the JNI boundary
//!   by `ExtraWindowActivity.forwardTouchEvent`.
//!
//! Both dispatchers inspect the action + button state and route into the
//! per-modality submodules:
//! - [`mouse`] for mouse buttons + wheel + drag.
//! - [`touch`] for touch tap state machine + two-finger right-click.
//! - [`trackpad`] for multi-touch centroid scroll synthesis.
//! - [`keyboard`] for hardware keys (entry points
//!   [`translate_key_event`] and [`translate_extra_key_event`]).
//!
//! IME / soft-keyboard composition will land in its own module
//! (see `docs/workarounds/deferred-soft-keyboard.md`).

pub(crate) mod keyboard;
pub(crate) mod mouse;
pub(crate) mod touch;
pub(crate) mod trackpad;

use android_activity::input::{Axis, MetaState, MotionAction, MotionEvent};
use gpui::{MouseButton, MouseMoveEvent, PlatformInput, point, px};

pub(crate) use keyboard::{translate_extra_key_event, translate_key_event};

/// Output of `translate_motion_event`. Touch interactions can need to
/// emit more than one synthetic input (the two-finger right-click path
/// emits Up(Left) + Down(Right) + Up(Right)) so the caller drains a
/// small vec rather than a single optional event.
pub(crate) type MotionInputs = Vec<PlatformInput>;

/// Java `MotionEvent.getActionMasked()` constants. We can't reuse
/// `android_activity::input::MotionAction` for the extra-window path
/// because that enum's constructor is private: `MotionEvent`s authored
/// from arbitrary JNI data only carry the integer.
const JAVA_ACTION_DOWN: i32 = 0;
const JAVA_ACTION_UP: i32 = 1;
const JAVA_ACTION_MOVE: i32 = 2;
const JAVA_ACTION_CANCEL: i32 = 3;
const JAVA_ACTION_POINTER_DOWN: i32 = 5;
const JAVA_ACTION_POINTER_UP: i32 = 6;
const JAVA_ACTION_HOVER_MOVE: i32 = 7;
const JAVA_ACTION_SCROLL: i32 = 8;

/// Convert an Android `MotionEvent` (touch / mouse / stylus) into one or
/// more gpui `PlatformInput`s. Coordinates arrive from Android in
/// physical pixels; gpui expects logical, so we divide by the active
/// `scale_factor` here.
pub(crate) fn translate_motion_event(
    event: &MotionEvent,
    scale_factor: f32,
) -> MotionInputs {
    if event.pointer_count() == 0 {
        return MotionInputs::new();
    }
    let primary = event.pointer_at_index(0);
    let position = point(
        px(primary.x() / scale_factor),
        px(primary.y() / scale_factor),
    );
    let modifiers = keyboard::modifiers_from_meta(event.meta_state());
    let button_state = event.button_state().0 as i32;
    let pressed_mouse_button = mouse::button_from_state(button_state);

    let mut out = MotionInputs::new();
    match event.action() {
        MotionAction::Down => {
            // If Android resolved a non-primary mouse / trackpad button
            // (right, middle, side-back, side-forward, or trackpad two-
            // finger tap surfacing as BUTTON_SECONDARY), emit the
            // corresponding Down and latch it in the touch state
            // machine so the matching Up resolves to the same button.
            // Skip touch's two-finger detection: Android already did it
            // for us.
            if let Some(button) = pressed_mouse_button
                && button != MouseButton::Left
            {
                let click_count = touch::next_click_count(button, position);
                touch::mark_non_primary_down(button);
                out.push(mouse::button_down(button, position, modifiers, click_count));
                return out;
            }
            // First finger down (touch) or mouse left button. Latch
            // position + time and emit Down(Left) immediately for
            // instant click feedback.
            let click_count = touch::next_click_count(MouseButton::Left, position);
            touch::record_primary_down(position);
            out.push(mouse::button_down(MouseButton::Left, position, modifiers, click_count));
        }
        MotionAction::PointerDown => {
            // Additional finger touched. If the primary finger is still
            // freshly-down within the two-finger window and hasn't
            // drifted (a true two-finger tap, not a finger added mid-
            // drag), cancel the in-flight left click and synthesize a
            // right-click sequence at the primary's position.
            if let Some(events) = touch::try_two_finger_right_click(position, modifiers) {
                out.extend(events);
            }
        }
        MotionAction::Up => {
            // Last finger up (or mouse button release). End any
            // multi-touch scroll gesture and resolve the latched click.
            trackpad::reset_primary();
            if let touch::UpOutcome::Emit(button) = touch::finalize_up() {
                out.push(mouse::button_up(button, position, modifiers));
            }
        }
        MotionAction::PointerUp => {
            // A non-last finger lifted. The two-finger gesture (if any)
            // already resolved at PointerDown; nothing to emit. Drop
            // the multi-touch scroll centroid so the next MOVE doesn't
            // compute a delta against a stale frame.
            trackpad::reset_primary();
        }
        MotionAction::Move => {
            if event.pointer_count() >= 2 {
                // Multi-touch drag: synthesize a ScrollWheelEvent from
                // the centroid delta. Samsung Book Cover trackpad fires
                // two-finger scroll as multi-pointer ACTION_MOVE, not
                // ACTION_SCROLL: we recognize the gesture ourselves.
                let mut sum_x = 0.0f32;
                let mut sum_y = 0.0f32;
                for i in 0..event.pointer_count() {
                    let p = event.pointer_at_index(i);
                    sum_x += p.x();
                    sum_y += p.y();
                }
                let n = event.pointer_count() as f32;
                let cur = (sum_x / n, sum_y / n);
                let update =
                    trackpad::primary_multi_touch_update(cur, position, modifiers, scale_factor);
                if let Some(cancel) = update.cancel_left {
                    out.push(cancel);
                    touch::reset_all();
                }
                if let Some(scroll) = update.scroll {
                    out.push(scroll);
                }
                return out;
            }
            trackpad::reset_primary();
            touch::update_drift(position);
            // Drag with a non-primary button held (right-drag, middle-
            // drag, side-button drag): report that button as the
            // pressed_button. Falls back to Left for touch drag (no
            // physical-button concept) and for plain mouse left-drag.
            let pressed = touch::current_non_primary()
                .or(pressed_mouse_button)
                .unwrap_or(MouseButton::Left);
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: Some(pressed),
                modifiers,
            }));
        }
        MotionAction::HoverMove => {
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }));
        }
        MotionAction::Cancel => {
            // End the gesture cleanly. Emit the up for whichever button
            // was held (or Left as a default touch-cancel) with
            // click_count=0 so listeners can distinguish a real click.
            let held = touch::current_non_primary().unwrap_or(MouseButton::Left);
            touch::reset_all();
            trackpad::reset_primary();
            out.push(PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: held,
                position,
                modifiers,
                click_count: 0,
            }));
        }
        MotionAction::Scroll => {
            let vscroll = primary.axis_value(Axis::Vscroll);
            let hscroll = primary.axis_value(Axis::Hscroll);
            if let Some(event) = mouse::wheel_scroll(vscroll, hscroll, position, modifiers) {
                out.push(event);
            }
        }
        _ => {}
    }
    out
}

/// Touch translator for events arriving on extra `SurfaceView`s (i.e.
/// secondary gpui windows hosted by `multi_window`). The primary path
/// uses [`translate_motion_event`] which consumes android-activity's
/// NDK-backed `MotionEvent`; this one takes the raw fields we marshal
/// across the JNI boundary in `ExtraWindowActivity.forwardTouchEvent`.
///
/// Handles the same input vocabulary as the primary translator: touch
/// DOWN/MOVE/UP, mouse hover, mouse-wheel + trackpad two-finger scroll,
/// and physical secondary-button (right-click) on mouse / trackpad.
///
/// Multi-touch right-click synthesis (touch two-finger tap → secondary)
/// is NOT mirrored here: Settings / Keymap / Themes don't surface a
/// context menu, so the extra cost wouldn't pay off until we ship a
/// window that actually wants it.
pub(crate) fn translate_extra_motion_event(
    action_masked: i32,
    _action_index: i32,
    meta_state: i32,
    button_state: i32,
    vscroll: f32,
    hscroll: f32,
    positions: &[(f32, f32, i32)],
    scale_factor: f32,
) -> MotionInputs {
    if positions.is_empty() {
        return Vec::new();
    }
    let (raw_x, raw_y, _id) = positions[0];
    let position = point(px(raw_x / scale_factor), px(raw_y / scale_factor));
    let modifiers = keyboard::modifiers_from_meta(MetaState(meta_state as u32));
    let pressed_button = mouse::button_from_state(button_state);

    let mut out = Vec::new();
    match action_masked {
        JAVA_ACTION_DOWN | JAVA_ACTION_POINTER_DOWN => {
            // Same shape as the primary translator's Down: non-primary
            // mouse / trackpad button (right, middle, navigate) emits
            // its own Down and latches HELD_NON_PRIMARY so the matching
            // Up resolves to the same button (Android reports
            // button_state=0 on Up, so we can't recover it from there).
            let button = pressed_button.unwrap_or(MouseButton::Left);
            let click_count = touch::next_click_count(button, position);
            if button != MouseButton::Left {
                touch::mark_non_primary_down(button);
            } else {
                touch::record_primary_down(position);
            }
            out.push(mouse::button_down(button, position, modifiers, click_count));
        }
        JAVA_ACTION_UP | JAVA_ACTION_POINTER_UP => {
            // End any multi-touch scroll and resolve the latched click.
            trackpad::reset_extra();
            if let touch::UpOutcome::Emit(button) = touch::finalize_up() {
                out.push(mouse::button_up(button, position, modifiers));
            }
        }
        JAVA_ACTION_CANCEL => {
            // Reset state, emit a non-click up for whichever button was
            // held so listeners can distinguish a real click.
            trackpad::reset_extra();
            let held = touch::current_non_primary().unwrap_or(MouseButton::Left);
            touch::reset_all();
            out.push(PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: held,
                position,
                modifiers,
                click_count: 0,
            }));
        }
        JAVA_ACTION_MOVE => {
            if positions.len() >= 2 {
                let cur = trackpad::pointer_centroid(positions);
                let update =
                    trackpad::extra_multi_touch_update(cur, position, modifiers, scale_factor);
                if let Some(cancel) = update.cancel_left {
                    out.push(cancel);
                }
                if let Some(scroll) = update.scroll {
                    out.push(scroll);
                }
                return out;
            }
            trackpad::reset_extra();
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: Some(pressed_button.unwrap_or(MouseButton::Left)),
                modifiers,
            }));
        }
        JAVA_ACTION_HOVER_MOVE => {
            // Mouse moved without a button held. The scrollbar autohide
            // state machine (`crates/ui/src/components/scrollbar.rs`)
            // listens for `MouseMoveEvent { pressed_button: None }` to
            // fade the thumb in on parent-region entry.
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }));
        }
        JAVA_ACTION_SCROLL => {
            if let Some(event) = mouse::wheel_scroll(vscroll, hscroll, position, modifiers) {
                out.push(event);
            }
        }
        _ => {}
    }
    out
}
