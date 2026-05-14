//! Pointer-capture event synthesis.
//!
//! When `MainActivity.kt` calls `decorView.requestPointerCapture()` and
//! it succeeds, the trackpad delivers raw `MotionEvent`s to our
//! `OnCapturedPointerListener` instead of being filtered through
//! Android's gesture-detector layer. Captured events arrive tagged
//! `SOURCE_TOUCHPAD` (not `SOURCE_MOUSE`) with full multi-touch state,
//! per-pointer `AXIS_RELATIVE_X/Y` deltas, and clean
//! `ACTION_BUTTON_PRESS` / `ACTION_BUTTON_RELEASE` framing.
//!
//! This module turns that raw stream into the gpui `PlatformInput`
//! vocabulary so the editor sees normal `MouseMove` / `MouseDown` /
//! `MouseUp` / `ScrollWheel` events. We keep our own cursor position
//! (the system cursor is hidden while capture is active) and update it
//! by accumulating per-event relative deltas. Single-finger motion
//! moves the cursor; two-finger motion synthesizes a scroll from the
//! centroid delta and pins the cursor in place.
//!
//! ## Event flow
//!
//! Kotlin builds a `CapturedEvent` from each captured `MotionEvent` and
//! calls `nativeOnCapturedPointer` (JNI). The Rust handler pushes the
//! event onto a static unbounded channel; the platform's main loop
//! drains the channel each iteration and feeds the synthesized
//! `PlatformInput`s into the primary window's `handle_input`. Cross-
//! thread because Kotlin's listener fires on the UI thread but
//! `handle_input` must run on the game thread.
//!
//! ## Gestures (matching desktop trackpad standards)
//!
//! Single-finger:
//! - Plain tap (DOWN → UP within `TAP_WINDOW`, motion < `TAP_MOTION_PX`):
//!   synthesize `MouseDown(Left)` + `MouseUp(Left)` at cursor — left
//!   click.
//! - Tap-tap-drag (per libinput/Windows Precision Touchpad standard):
//!   plain tap, then a second `ACTION_DOWN` within `TAP_DRAG_WINDOW`
//!   of the first tap-up near the same position. On the first MOVE
//!   past `TAP_DRAG_SLOP`, emit `MouseDown(Left)` at the tap anchor
//!   and treat subsequent motion as click-and-drag. ACTION_UP ends
//!   the drag. This is how text selection / rubber-band / drag-and-
//!   drop is invoked on every major desktop trackpad spec.
//! - `ACTION_BUTTON_PRESS` (Samsung's driver-recognized drag): emit
//!   `MouseDown(Left)`, follow with MouseMove(Left held), release on
//!   `ACTION_BUTTON_RELEASE`. Coexists with tap-tap-drag for
//!   trackpads that don't emit BUTTON_PRESS reliably in capture mode.
//!
//! Two-finger:
//! - Quick tap-tap (both fingers down + up within `TWO_FINGER_TAP_WINDOW`
//!   with low motion): right-click at cursor.
//! - Move: scroll. Never selection — text selection is a single-finger
//!   gesture per the standard, no matter which finger combination.
//!
//! There is intentionally no "hold finger 1 + drag finger 2"
//! synthesis. That pattern isn't a standard desktop trackpad gesture
//! (no major spec defines it) and conflicts with two-finger scroll
//! detection.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection,
    Pixels, PlatformInput, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, point, px,
};
use jni::objects::{JFloatArray, JObject, JString};

/// Java `MotionEvent.getActionMasked()` constants. Mirrors the ones in
/// `events.rs` for the same JNI reason: `MotionAction` from the NDK
/// crate has a private constructor.
const JAVA_ACTION_DOWN: i32 = 0;
const JAVA_ACTION_UP: i32 = 1;
const JAVA_ACTION_MOVE: i32 = 2;
const JAVA_ACTION_CANCEL: i32 = 3;
const JAVA_ACTION_POINTER_DOWN: i32 = 5;
const JAVA_ACTION_POINTER_UP: i32 = 6;
const JAVA_ACTION_BUTTON_PRESS: i32 = 11;
const JAVA_ACTION_BUTTON_RELEASE: i32 = 12;

/// Android `MotionEvent.BUTTON_*` constants. Mirrors `events/mouse.rs`.
const ANDROID_BUTTON_PRIMARY: i32 = 1 << 0;
const ANDROID_BUTTON_SECONDARY: i32 = 1 << 1;
const ANDROID_BUTTON_TERTIARY: i32 = 1 << 2;
const ANDROID_BUTTON_BACK: i32 = 1 << 3;
const ANDROID_BUTTON_FORWARD: i32 = 1 << 4;

/// Tap detection: a single-finger `ACTION_DOWN` → `ACTION_UP` within
/// this time, with less than `TAP_MOTION_PX` of accumulated motion,
/// counts as a click and synthesizes `MouseDown` + `MouseUp(Left)`.
const TAP_WINDOW: Duration = Duration::from_millis(200);
const TAP_MOTION_PX: f32 = 6.0;

/// Two-finger tap → right-click detection. The second finger must
/// land within this window of the first and the gesture must not
/// have drifted past `TWO_FINGER_TAP_MOTION_PX` yet. Arms the right-
/// click on `ACTION_POINTER_DOWN`; the click only fires on
/// `ACTION_POINTER_UP` so a two-finger SCROLL (which moves before
/// lifting) disarms via the motion-accum path and doesn't accidentally
/// pop a context menu mid-scroll. Matches `events/touch.rs`'s
/// touchscreen two-finger-tap behavior for consistency.
const TWO_FINGER_TAP_WINDOW: Duration = Duration::from_millis(300);
const TWO_FINGER_TAP_MOTION_PX: f32 = 6.0;

/// Tap-tap-drag window: time from the first tap's UP to the second
/// tap's DOWN. Per libinput's tap state machine, 300ms is the
/// implementation-defined timeout. If the second touch lands within
/// this window AND near the first tap's position, it qualifies as the
/// start of a tap-tap-drag rather than an unrelated second tap.
const TAP_DRAG_WINDOW: Duration = Duration::from_millis(300);

/// Tap-tap-drag position slop: the second touch must land within
/// this many logical pixels of the first tap's position to count as
/// part of the same gesture. Matches the click-position slop used by
/// our existing multi-click detection so feel is consistent.
const TAP_DRAG_POSITION_SLOP_PX: f32 = 6.0;

/// Tap-tap-drag motion-to-engage: how far the finger must move after
/// the second-tap DOWN before we commit to drag mode (and emit
/// `MouseDown(Left)`). Below this the system can still resolve the
/// gesture as a regular double-tap if the user lifts the finger
/// without dragging.
const TAP_DRAG_SLOP_PX: f32 = 2.0;


/// Captured `MotionEvent` shape marshaled across JNI. One per event.
/// Pointer fields are indexed 0..pointer_count. `cursor_physical_*`
/// is Kotlin's authoritative cursor position in physical pixels
/// (decorView coordinate space) — Kotlin maintains it because the
/// software cursor View has to be positioned in the same space.
#[derive(Debug)]
pub(crate) struct CapturedEvent {
    pub action_masked: i32,
    pub source: i32,
    pub button_state: i32,
    pub pointer_count: usize,
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    pub rxs: Vec<f32>,
    pub rys: Vec<f32>,
    pub vscroll: f32,
    pub hscroll: f32,
    pub cursor_physical_x: f32,
    pub cursor_physical_y: f32,
}

static EVENT_TX: Mutex<Option<mpsc::UnboundedSender<CapturedEvent>>> = Mutex::new(None);

/// Construct a fresh sender/receiver pair. Returns the receiver for
/// the platform to drain; the sender lives in `EVENT_TX` for the JNI
/// thread to push into. Safe to call multiple times (Activity-
/// recreation idempotent) — each call drops the previous sender.
pub(crate) fn init_event_channel() -> mpsc::UnboundedReceiver<CapturedEvent> {
    let (tx, rx) = mpsc::unbounded();
    *EVENT_TX.lock().unwrap() = Some(tx);
    rx
}

/// Synthesizer state. Lives in a thread-local on the game thread (the
/// only consumer of captured events) so we avoid the cost of a mutex
/// on the hot translation path. Cursor position is owned by Kotlin
/// and threaded through every event so the on-screen cursor View
/// and the gpui-side cursor never drift.
struct SynthState {
    /// Set when `ACTION_BUTTON_PRESS` fired; we hold the button until
    /// `ACTION_BUTTON_RELEASE`. Used to populate `pressed_button` on
    /// subsequent move events.
    button_held: Option<MouseButton>,
    /// Latched on `ACTION_DOWN` (first finger). Cleared on Up, Cancel,
    /// or when motion exceeds `TAP_MOTION_PX`. Used to synthesize a
    /// click on a clean Down→Up with minimal motion.
    primary_down: Option<TapAnchor>,
    /// When the first finger of the current gesture landed. Distinct
    /// from `primary_down` because the latter clears on small cursor
    /// motion (to prevent tap-on-up after a drag) but this stays
    /// stable through the whole gesture so drag-lock detection can
    /// measure how long the first finger has been on the pad before
    /// the second one landed. Cleared only on UP / CANCEL.
    first_finger_at: Option<Instant>,
    /// Accumulated cursor motion since the last tap anchor was set.
    /// Compared to `TAP_MOTION_PX` to decide whether a Down→Up
    /// sequence is a tap or a drag.
    motion_accum: f32,
    /// True when more than one finger is currently on the pad, so
    /// motion events synthesize scroll instead of cursor movement.
    in_multi_touch: bool,
    /// Set when `ACTION_POINTER_DOWN` (second finger) landed within
    /// the two-finger-tap window without significant motion. The
    /// actual right-click fires on `ACTION_POINTER_UP` so an in-
    /// flight two-finger SCROLL (which exceeds the motion threshold
    /// before lifting) clears this and doesn't synthesize a click.
    right_click_armed: bool,
    /// Tap-tap-drag tracking: when the most recent single-finger tap
    /// ended (ACTION_UP that synthesized a click). The next
    /// ACTION_DOWN within `TAP_DRAG_WINDOW` of this point near
    /// `last_tap_position` arms the drag.
    last_tap_at: Option<Instant>,
    /// Position of the most recent tap (in logical pixels). The
    /// second-tap ACTION_DOWN must land within `TAP_DRAG_POSITION_SLOP_PX`
    /// of this to qualify as part of a tap-tap-drag gesture.
    last_tap_position: Point<Pixels>,
    /// True between the second-tap ACTION_DOWN and the first MOVE
    /// past `TAP_DRAG_SLOP_PX`. If the user lifts the finger without
    /// moving (a plain double-tap), this clears and the second tap
    /// just becomes a second click. If they move past the slop, we
    /// commit to drag mode.
    tap_drag_pending: bool,
    /// True after `tap_drag_pending` resolved into a drag (i.e. we
    /// emitted `MouseDown(Left)` and are now in drag mode). MouseMove
    /// fires with Left held until ACTION_UP, which emits the matching
    /// MouseUp.
    tap_drag_active: bool,
    /// Last cursor position seen, in logical pixels. Kept so we can
    /// emit `MouseUp` / synthetic-tap events at the correct location
    /// even when the triggering event itself doesn't carry an updated
    /// cursor (e.g. `ACTION_BUTTON_RELEASE` may arrive without a
    /// fresh relative delta).
    last_cursor: Point<Pixels>,
}

struct TapAnchor {
    when: Instant,
    cursor: Point<Pixels>,
}

impl SynthState {
    fn new() -> Self {
        Self {
            button_held: None,
            primary_down: None,
            first_finger_at: None,
            motion_accum: 0.0,
            in_multi_touch: false,
            right_click_armed: false,
            last_tap_at: None,
            last_tap_position: Point::default(),
            tap_drag_pending: false,
            tap_drag_active: false,
            last_cursor: Point::default(),
        }
    }
}

thread_local! {
    static STATE: RefCell<SynthState> = RefCell::new(SynthState::new());
}

/// Push a captured event from the JVM listener thread onto the channel.
/// The game thread drains and synthesizes.
fn dispatch(event: CapturedEvent) {
    let guard = EVENT_TX.lock().unwrap();
    let Some(tx) = guard.as_ref() else {
        log::warn!("captured_pointer: event arrived before init_event_channel");
        return;
    };
    if let Err(err) = tx.unbounded_send(event) {
        log::warn!("captured_pointer: dispatch failed: {err:#}");
    }
}

/// Translate one captured event into zero or more `PlatformInput`s,
/// updating internal state. Called on the game thread from the
/// platform's main loop drain. `scale_factor` is the surface's
/// physical-to-logical multiplier; we divide Kotlin's physical-pixel
/// cursor position by it to get logical pixels for gpui events.
pub(crate) fn translate(
    event: CapturedEvent,
    scale_factor: f32,
) -> Vec<PlatformInput> {
    let modifiers = Modifiers::default();
    let pressed_mouse_button = button_from_state(event.button_state);
    let scale = if scale_factor > 0.0 { scale_factor } else { 1.0 };
    let cursor = point(
        px(event.cursor_physical_x / scale),
        px(event.cursor_physical_y / scale),
    );

    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.last_cursor = cursor;
        let mut out = Vec::new();

        match event.action_masked {
            JAVA_ACTION_DOWN => {
                // First finger landed. Two roles:
                //
                // 1. Tap-tap-drag detection: if this DOWN follows a
                //    recent tap (within TAP_DRAG_WINDOW) near the same
                //    position, this is the second touch of a
                //    tap-tap-drag gesture. Arm tap_drag_pending; the
                //    first MOVE past slop will commit to drag mode.
                // 2. Single-tap detection: latch the anchor so the
                //    matching UP can synthesize a click if motion
                //    stays low.
                let now = Instant::now();
                let qualifies_as_tap_drag = state
                    .last_tap_at
                    .map(|t| now.duration_since(t) < TAP_DRAG_WINDOW)
                    .unwrap_or(false)
                    && (cursor - state.last_tap_position).magnitude()
                        <= TAP_DRAG_POSITION_SLOP_PX as f64;
                state.tap_drag_pending = qualifies_as_tap_drag;
                state.last_tap_at = None;
                state.primary_down = Some(TapAnchor {
                    when: now,
                    cursor,
                });
                state.first_finger_at = Some(now);
                state.motion_accum = 0.0;
                state.in_multi_touch = false;
                state.right_click_armed = false;
            }
            JAVA_ACTION_POINTER_DOWN => {
                // Second finger landed. Don't commit to a mode yet —
                // wait for accumulated motion to discriminate drag-lock
                // (asymmetric) from scroll (symmetric). Reset
                // accumulators; arm right-click for the case where
                // POINTER_UP arrives before any meaningful motion.
                state.right_click_armed = true;
                state.primary_down = None;
                state.in_multi_touch = true;
                // Cancel any in-flight tap-tap-drag if the user lands
                // a second finger mid-gesture — the gesture is now
                // multi-touch, not a tap-drag.
                state.tap_drag_pending = false;
            }
            JAVA_ACTION_MOVE => {
                if event.pointer_count >= 2 || state.in_multi_touch {
                    // Two-or-more-finger move is ALWAYS scroll. No
                    // drag-select branch here — text selection is a
                    // single-finger gesture per the desktop standard
                    // (Windows Precision Touchpad, libinput, macOS).
                    // The centroid delta drives ScrollDelta::Pixels;
                    // sign-flipped for natural scrolling (finger down
                    // = content moves down).
                    let n = event.pointer_count.max(1) as f32;
                    let sum_rx: f32 = event.rxs.iter().sum();
                    let sum_ry: f32 = event.rys.iter().sum();
                    let dx = sum_rx / n / scale;
                    let dy = sum_ry / n / scale;
                    // Any meaningful motion disarms right-click
                    // (gesture is no longer a quick two-finger tap).
                    if (dx * dx + dy * dy).sqrt() > TWO_FINGER_TAP_MOTION_PX {
                        state.right_click_armed = false;
                    }
                    if dx != 0.0 || dy != 0.0 {
                        out.push(PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position: cursor,
                            delta: ScrollDelta::Pixels(point(px(-dx), px(-dy))),
                            modifiers,
                            touch_phase: TouchPhase::Moved,
                        }));
                    }
                } else if event.pointer_count == 1 {
                    // Single-finger move. Cursor is already at the
                    // post-delta position (Kotlin updated it before
                    // forwarding). Track motion accumulator so tap
                    // synthesis gets cancelled if the finger drifts
                    // past the tap threshold.
                    let dx = *event.rxs.first().unwrap_or(&0.0);
                    let dy = *event.rys.first().unwrap_or(&0.0);
                    state.motion_accum += (dx * dx + dy * dy).sqrt();
                    if state.motion_accum > TAP_MOTION_PX {
                        state.primary_down = None;
                    }

                    // Tap-tap-drag commit: if a previous tap armed
                    // `tap_drag_pending` on this DOWN, the first motion
                    // past slop transitions us into drag mode. Emit
                    // MouseDown(Left) at the original tap position
                    // (the anchor, where the user expected the drag
                    // to start from) and subsequent MouseMove events
                    // grow the drag.
                    if state.tap_drag_pending
                        && state.motion_accum > TAP_DRAG_SLOP_PX
                        && state.button_held.is_none()
                    {
                        state.tap_drag_pending = false;
                        state.tap_drag_active = true;
                        state.button_held = Some(MouseButton::Left);
                        out.push(PlatformInput::MouseDown(MouseDownEvent {
                            button: MouseButton::Left,
                            position: state.last_tap_position,
                            modifiers,
                            click_count: 1,
                            first_mouse: false,
                        }));
                    }

                    out.push(PlatformInput::MouseMove(MouseMoveEvent {
                        position: cursor,
                        pressed_button: state.button_held,
                        modifiers,
                    }));
                }
            }
            JAVA_ACTION_UP => {
                // Last finger lifted. Three cases:
                //
                // 1. tap_drag_active: we were dragging. Release the
                //    held button so gpui ends the selection cleanly.
                // 2. primary_down anchor still fresh + low motion:
                //    synthesize a tap click. Record last_tap_at +
                //    position so a follow-up DOWN within
                //    TAP_DRAG_WINDOW can arm tap-tap-drag.
                // 3. Neither (e.g. user dragged the cursor a long way
                //    without crossing into tap-drag mode): no click,
                //    just clear state.
                state.in_multi_touch = false;
                if state.tap_drag_active {
                    if let Some(button) = state.button_held.take() {
                        out.push(PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position: cursor,
                            modifiers,
                            click_count: 1,
                        }));
                    }
                    state.tap_drag_active = false;
                } else if let Some(anchor) = state.primary_down.take()
                    && anchor.when.elapsed() < TAP_WINDOW
                    && state.motion_accum <= TAP_MOTION_PX
                {
                    out.push(PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position: cursor,
                        modifiers,
                        click_count: 1,
                        first_mouse: false,
                    }));
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position: cursor,
                        modifiers,
                        click_count: 1,
                    }));
                    // Record this tap so a subsequent ACTION_DOWN
                    // within `TAP_DRAG_WINDOW` can arm tap-tap-drag
                    // for text selection.
                    state.last_tap_at = Some(Instant::now());
                    state.last_tap_position = cursor;
                }
                state.motion_accum = 0.0;
                state.right_click_armed = false;
                state.first_finger_at = None;
                state.tap_drag_pending = false;
            }
            JAVA_ACTION_POINTER_UP => {
                // One of the fingers in a multi-touch gesture lifted.
                // If it was a two-finger tap (we armed right-click on
                // POINTER_DOWN and the gesture never crossed the
                // motion threshold), fire the right-click now.
                if state.right_click_armed {
                    out.push(PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Right,
                        position: cursor,
                        modifiers,
                        click_count: 1,
                        first_mouse: false,
                    }));
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Right,
                        position: cursor,
                        modifiers,
                        click_count: 1,
                    }));
                    state.right_click_armed = false;
                }
                if event.pointer_count <= 2 {
                    state.in_multi_touch = false;
                }
            }
            JAVA_ACTION_BUTTON_PRESS => {
                let button = pressed_mouse_button.unwrap_or(MouseButton::Left);
                state.button_held = Some(button);
                state.primary_down = None;
                out.push(PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position: cursor,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }));
            }
            JAVA_ACTION_BUTTON_RELEASE => {
                if let Some(button) = state.button_held.take() {
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button,
                        position: cursor,
                        modifiers,
                        click_count: 1,
                    }));
                }
            }
            JAVA_ACTION_CANCEL => {
                if let Some(button) = state.button_held.take() {
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button,
                        position: cursor,
                        modifiers,
                        click_count: 0,
                    }));
                }
                state.primary_down = None;
                state.first_finger_at = None;
                state.motion_accum = 0.0;
                state.in_multi_touch = false;
                state.right_click_armed = false;
                state.tap_drag_pending = false;
                state.tap_drag_active = false;
            }
            _ => {}
        }
        out
    })
}

fn button_from_state(state: i32) -> Option<MouseButton> {
    if state & ANDROID_BUTTON_PRIMARY != 0 {
        Some(MouseButton::Left)
    } else if state & ANDROID_BUTTON_TERTIARY != 0 {
        Some(MouseButton::Middle)
    } else if state & ANDROID_BUTTON_SECONDARY != 0 {
        Some(MouseButton::Right)
    } else if state & ANDROID_BUTTON_BACK != 0 {
        Some(MouseButton::Navigate(NavigationDirection::Back))
    } else if state & ANDROID_BUTTON_FORWARD != 0 {
        Some(MouseButton::Navigate(NavigationDirection::Forward))
    } else {
        None
    }
}

// JNI sinks --------------------------------------------------------------

/// Probe sink (kept alongside the structured one for debug). Logs a
/// stringified summary of each captured event.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeOnCapturedPointerProbe<
    'local,
>(
    mut env: jni::JNIEnv<'local>,
    _bridge: JObject<'local>,
    summary: JString<'local>,
) {
    let summary: String = match env.get_string(&summary) {
        Ok(s) => s.into(),
        Err(err) => {
            log::warn!("captured_pointer: failed to decode summary: {err:#}");
            return;
        }
    };
    log::info!("captured_pointer: {summary}");
}

/// Structured sink. Marshals Kotlin-side `MotionEvent` fields, builds a
/// `CapturedEvent`, dispatches onto the channel for the game thread to
/// drain.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeOnCapturedPointer<
    'local,
>(
    env: jni::JNIEnv<'local>,
    _bridge: JObject<'local>,
    action_masked: i32,
    source: i32,
    button_state: i32,
    pointer_count: i32,
    xs: JFloatArray<'local>,
    ys: JFloatArray<'local>,
    rxs: JFloatArray<'local>,
    rys: JFloatArray<'local>,
    vscroll: f32,
    hscroll: f32,
    cursor_physical_x: f32,
    cursor_physical_y: f32,
) {
    let n = pointer_count.max(0) as usize;
    let xs = read_floats(&env, &xs, n);
    let ys = read_floats(&env, &ys, n);
    let rxs = read_floats(&env, &rxs, n);
    let rys = read_floats(&env, &rys, n);
    dispatch(CapturedEvent {
        action_masked,
        source,
        button_state,
        pointer_count: n,
        xs,
        ys,
        rxs,
        rys,
        vscroll,
        hscroll,
        cursor_physical_x,
        cursor_physical_y,
    });
}

fn read_floats<'local>(
    env: &jni::JNIEnv<'local>,
    array: &JFloatArray<'local>,
    expected_len: usize,
) -> Vec<f32> {
    let mut buf = vec![0.0f32; expected_len];
    if expected_len == 0 {
        return buf;
    }
    if let Err(err) = env.get_float_array_region(array, 0, buf.as_mut_slice()) {
        log::warn!("captured_pointer: get_float_array_region: {err:#}");
        buf.clear();
    }
    buf
}
