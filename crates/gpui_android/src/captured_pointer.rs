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
//! ## Tap detection
//!
//! Samsung's trackpad in capture mode emits `ACTION_BUTTON_PRESS` only
//! when its driver recognises a tap-and-hold-and-drag pattern. A plain
//! single tap is just `ACTION_DOWN` → (~no motion~) → `ACTION_UP`. We
//! synthesize a click in that case by latching the down position +
//! time and emitting `MouseDown` + `MouseUp` at the cursor position on
//! Up if total motion < `TAP_MOTION_PX` and duration < `TAP_WINDOW`.

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
    /// Accumulated cursor motion since the last tap anchor was set.
    /// Compared to `TAP_MOTION_PX` to decide whether a Down→Up
    /// sequence is a tap or a drag.
    motion_accum: f32,
    /// True when more than one finger is currently on the pad, so
    /// motion events synthesize scroll instead of cursor movement.
    in_multi_touch: bool,
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
            motion_accum: 0.0,
            in_multi_touch: false,
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
                // First finger landed. Latch a tap anchor so a quick
                // Down→Up with minimal motion synthesizes a click.
                state.primary_down = Some(TapAnchor {
                    when: Instant::now(),
                    cursor,
                });
                state.motion_accum = 0.0;
                state.in_multi_touch = false;
            }
            JAVA_ACTION_POINTER_DOWN => {
                // Additional finger; we're now in multi-touch territory.
                // Cancel any in-flight tap detection because the gesture
                // is no longer a simple tap.
                state.primary_down = None;
                state.in_multi_touch = true;
            }
            JAVA_ACTION_MOVE => {
                if event.pointer_count >= 2 || state.in_multi_touch {
                    // Two-or-more-finger move → synthesize scroll from
                    // the centroid delta. Average per-pointer rx/ry so
                    // the scroll vector follows both fingers' motion.
                    // Convert physical-pixel deltas to logical for the
                    // ScrollWheelEvent.
                    let n = event.pointer_count.max(1) as f32;
                    let sum_rx: f32 = event.rxs.iter().sum();
                    let sum_ry: f32 = event.rys.iter().sum();
                    let dx = sum_rx / n / scale;
                    let dy = sum_ry / n / scale;
                    if dx != 0.0 || dy != 0.0 {
                        out.push(PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position: cursor,
                            delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
                            modifiers,
                            touch_phase: TouchPhase::Moved,
                        }));
                    }
                } else if event.pointer_count == 1 {
                    // Single-finger move → cursor is already at the
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
                    out.push(PlatformInput::MouseMove(MouseMoveEvent {
                        position: cursor,
                        pressed_button: state.button_held,
                        modifiers,
                    }));
                }
            }
            JAVA_ACTION_UP => {
                // Last finger lifted. If we still have a fresh
                // primary_down anchor (no BUTTON_PRESS fired in
                // between, no excessive motion), emit a synthetic
                // click at the current cursor position.
                state.in_multi_touch = false;
                if let Some(anchor) = state.primary_down.take()
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
                }
                state.motion_accum = 0.0;
            }
            JAVA_ACTION_POINTER_UP => {
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
                state.motion_accum = 0.0;
                state.in_multi_touch = false;
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
