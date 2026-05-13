use std::cell::Cell;
use std::time::{Duration, Instant};

use android_activity::input::{
    Axis, KeyAction, KeyEvent, Keycode, MetaState, MotionAction, MotionEvent,
};
use gpui::{
    Capslock, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Point, ScrollDelta,
    ScrollWheelEvent, TouchPhase, point, px,
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
    static PRIMARY_DOWN: Cell<Option<(Instant, Point<gpui::Pixels>)>> = const { Cell::new(None) };
    /// Set when a two-finger tap fired Right-click. The subsequent
    /// `Up`/`PointerUp` events for the gesture should NOT emit Up(Left).
    static RIGHT_CLICK_FIRED: Cell<bool> = const { Cell::new(false) };
    /// Last centroid (avg of all active pointer positions, in raw pixel
    /// coords pre-scale-divide) of an ongoing multi-touch gesture, used to
    /// synthesize a `ScrollWheelEvent` from `ACTION_MOVE` deltas. Samsung
    /// Book Cover Keyboard's trackpad — and presumably other Android
    /// trackpads — sends two-finger scroll as raw multi-pointer
    /// `ACTION_MOVE` events on `SOURCE_TOUCHSCREEN`, NOT as
    /// `ACTION_SCROLL`. Same shape VNC + X11 hit on Android. The OS
    /// expects the app to recognize the gesture and synthesize the scroll
    /// itself. `None` when no multi-touch gesture is in flight.
    static MULTI_TOUCH_CENTROID: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// Mirror for the extra-window translator (`translate_extra_motion_event`).
    /// Separate cell so a gesture in one window doesn't leak its prev-frame
    /// state into the other.
    static EXTRA_MULTI_TOUCH_CENTROID: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
}

/// Centroid (mean position) of all active pointers. Used as the reference
/// point for multi-touch scroll delta computation — averages out small
/// per-finger jitter and naturally lifts/drops as fingers join / leave the
/// gesture.
fn pointer_centroid(positions: &[(f32, f32, i32)]) -> (f32, f32) {
    let n = positions.len() as f32;
    let sum_x: f32 = positions.iter().map(|(x, _, _)| *x).sum();
    let sum_y: f32 = positions.iter().map(|(_, y, _)| *y).sum();
    (sum_x / n, sum_y / n)
}

/// Output of `translate_motion_event`. Touch interactions can need to
/// emit more than one synthetic input (long-press needs Up(Left)
/// click_count=0 + Down(Right) + Up(Right)) so the caller drains a small
/// vec rather than a single optional event.
pub(crate) type MotionInputs = Vec<PlatformInput>;

/// Convert an Android `KeyEvent` into a gpui `PlatformInput`.
///
/// Returns `None` when the event isn't translatable (e.g. `KeyAction::Multiple`,
/// which is reserved for synthesized character sequences from soft keyboards we
/// don't currently support).
pub(crate) fn translate_key_event(event: &KeyEvent) -> Option<PlatformInput> {
    let action = event.action();
    let keycode = event.key_code();
    let modifiers = modifiers_from_meta(event.meta_state());

    if is_modifier_key(keycode) {
        return Some(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers,
            capslock: capslock_from_meta(event.meta_state()),
        }));
    }

    let keystroke = build_keystroke(keycode, modifiers);

    match action {
        KeyAction::Down => Some(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: event.repeat_count() > 0,
            prefer_character_input: false,
        })),
        KeyAction::Up => Some(PlatformInput::KeyUp(KeyUpEvent { keystroke })),
        _ => None,
    }
}

/// Convert an Android `MotionEvent` (touch / mouse / stylus) into one or
/// more gpui `PlatformInput`s. Touch model:
///   - **Single-finger tap / drag** → left mouse click / drag (selection
///     in editor & terminal works as expected)
///   - **Two-finger tap** → right click — the standard tablet/VNC pattern
///     for invoking `on_secondary_mouse_down` (project panel context menu,
///     terminal context menu, tab close menu, etc.) without colliding
///     with text selection
///   - **Long single-finger press** → just keeps the left button held;
///     no synthetic right-click. (Earlier versions did long-press →
///     right-click; that interfered with text selection because the
///     selection-and-context-menu gestures look identical.)
///
/// Coordinates arrive from Android in physical pixels; gpui expects
/// logical, so we divide by the active scale_factor here.
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
    let modifiers = modifiers_from_meta(event.meta_state());

    // For mouse / trackpad input, Android pre-resolves multi-finger
    // gestures and physical buttons into `button_state`. A two-finger
    // tap on the Galaxy Book Cover trackpad arrives here as a single
    // pointer with `BUTTON_SECONDARY` set; physical right-click on a
    // USB mouse arrives the same way. Honor it directly so the user
    // doesn't have to do anything funny on a trackpad.
    let secondary_button = event.button_state().secondary();

    let mut out = MotionInputs::new();
    match event.action() {
        MotionAction::Down => {
            if secondary_button {
                // Trackpad two-finger tap or mouse right-click. Skip
                // the touch-style two-finger detection — Android did it
                // for us. Don't latch PRIMARY_DOWN; we don't want a
                // subsequent Up to emit Up(Left).
                RIGHT_CLICK_FIRED.with(|cell| cell.set(true));
                out.push(PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Right,
                    position,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }));
                return out;
            }
            // First finger down. Latch position+time and emit Down(Left)
            // immediately for instant click feedback.
            PRIMARY_DOWN.with(|cell| cell.set(Some((Instant::now(), position))));
            RIGHT_CLICK_FIRED.with(|cell| cell.set(false));
            // `first_mouse: false` because there's no window-focus concept on
            // Android; the app is always focused when it receives input.
            // Setting `true` would make every click look like a focus-the-
            // window-first click, which `ClickEvent::first_focus` returns as
            // true — and listeners like ProjectPanel's on_click bail on a
            // "first focus" click, so files would never open / folders would
            // never expand.
            out.push(PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }));
        }
        MotionAction::PointerDown => {
            // Additional finger touched. If the primary finger is still
            // freshly-down within TWO_FINGER_WINDOW and hasn't drifted
            // (i.e. user did a true two-finger tap, not a finger added
            // mid-drag), cancel the in-flight left click and synthesize
            // a right-click sequence at the primary's position.
            let primary_state = PRIMARY_DOWN.with(|cell| cell.get());
            let qualifies = primary_state
                .map(|(t, p)| {
                    t.elapsed() < TWO_FINGER_WINDOW
                        && (position - p).magnitude() <= TWO_FINGER_SLOP
                })
                .unwrap_or(false);
            if qualifies {
                let primary_pos = primary_state.map(|(_, p)| p).unwrap_or(position);
                // Cancel the left-click without firing on_click...
                out.push(PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: primary_pos,
                    modifiers,
                    click_count: 0,
                }));
                // ...then synthesize a right-click at the primary's spot.
                out.push(PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Right,
                    position: primary_pos,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }));
                out.push(PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Right,
                    position: primary_pos,
                    modifiers,
                    click_count: 1,
                }));
                RIGHT_CLICK_FIRED.with(|cell| cell.set(true));
                PRIMARY_DOWN.with(|cell| cell.set(None));
            }
        }
        MotionAction::Up => {
            // Last finger up (or mouse button release). If a right-click
            // sequence was emitted on Down (touch two-finger or trackpad/
            // mouse secondary), close it with Up(Right). Otherwise close
            // the normal Up(Left). End any multi-touch scroll gesture.
            MULTI_TOUCH_CENTROID.with(|cell| cell.set(None));
            let fired = RIGHT_CLICK_FIRED.with(|cell| cell.take());
            let had_primary = PRIMARY_DOWN.with(|cell| cell.take()).is_some();
            if fired {
                // Pair the Down(Right) we emitted earlier with Up(Right)
                // so on_secondary_mouse_down sees a complete click. For
                // the touch two-finger path we already emitted both Down
                // and Up of Right at PointerDown time and `had_primary`
                // is false there; we only need this branch for trackpad/
                // mouse secondary button releases.
                if !had_primary {
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Right,
                        position,
                        modifiers,
                        click_count: 1,
                    }));
                }
            } else if had_primary {
                out.push(PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                }));
            }
        }
        MotionAction::PointerUp => {
            // A non-last finger lifted. The two-finger gesture (if any)
            // already resolved at PointerDown; nothing to emit here.
            // Drop the multi-touch scroll centroid so the next MOVE
            // doesn't compute a delta against a stale frame (the
            // remaining pointers' centroid has just jumped).
            MULTI_TOUCH_CENTROID.with(|cell| cell.set(None));
        }
        MotionAction::Move => {
            // Multi-touch drag (two+ fingers moving across the surface).
            // Synthesize a ScrollWheelEvent from the centroid delta —
            // Samsung Book Cover trackpad fires two-finger scroll as
            // multi-pointer ACTION_MOVE, not ACTION_SCROLL, and we have
            // to recognize the gesture ourselves. See the
            // `MULTI_TOUCH_CENTROID` thread-local doc for context.
            if event.pointer_count() >= 2 {
                let mut sum_x = 0.0f32;
                let mut sum_y = 0.0f32;
                for i in 0..event.pointer_count() {
                    let p = event.pointer_at_index(i);
                    sum_x += p.x();
                    sum_y += p.y();
                }
                let n = event.pointer_count() as f32;
                let cur = (sum_x / n, sum_y / n);
                let prev = MULTI_TOUCH_CENTROID.with(|cell| cell.replace(Some(cur)));
                // First multi-touch frame in the gesture. Cancel any
                // in-flight Left press from the original single-finger
                // Down so gpui doesn't see a stuck button while we
                // emit scrolls. click_count=0 means "not a click".
                if prev.is_none() {
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count: 0,
                    }));
                    PRIMARY_DOWN.with(|cell| cell.set(None));
                    RIGHT_CLICK_FIRED.with(|cell| cell.set(false));
                }
                if let Some((lx, ly)) = prev {
                    let dx = cur.0 - lx;
                    let dy = cur.1 - ly;
                    if dx != 0.0 || dy != 0.0 {
                        out.push(PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position,
                            delta: ScrollDelta::Pixels(point(
                                px(dx / scale_factor),
                                px(dy / scale_factor),
                            )),
                            modifiers,
                            touch_phase: TouchPhase::Moved,
                        }));
                    }
                }
                return out;
            }
            MULTI_TOUCH_CENTROID.with(|cell| cell.set(None));
            // Track drift so the two-finger window logic sees an
            // up-to-date "moved past slop" state (we don't strictly need
            // this since PointerDown checks distance from the original
            // anchor, but keeping the latch consistent simplifies
            // reasoning).
            PRIMARY_DOWN.with(|cell| {
                if let Some((t, p)) = cell.get() {
                    if (position - p).magnitude() > TWO_FINGER_SLOP {
                        cell.set(None);
                    } else {
                        cell.set(Some((t, p)));
                    }
                }
            });
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: Some(MouseButton::Left),
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
            PRIMARY_DOWN.with(|cell| cell.set(None));
            RIGHT_CLICK_FIRED.with(|cell| cell.set(false));
            MULTI_TOUCH_CENTROID.with(|cell| cell.set(None));
            out.push(PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 0,
            }));
        }
        MotionAction::Scroll => {
            // Mouse wheel + trackpad two-finger scroll arrives as Vscroll/Hscroll
            // axes in `lines`. gpui's ScrollDelta::Lines uses the same unit, but
            // Android reports +Y for "up" (away from user); gpui expects +Y to
            // scroll content up (the wheel-rotates-toward-you convention), so we
            // negate Vscroll so trackpad-down scrolls content down.
            let vscroll = primary.axis_value(Axis::Vscroll);
            let hscroll = primary.axis_value(Axis::Hscroll);
            if vscroll != 0.0 || hscroll != 0.0 {
                out.push(PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position,
                    delta: ScrollDelta::Lines(point(hscroll, -vscroll)),
                    modifiers,
                    touch_phase: TouchPhase::Moved,
                }));
            }
        }
        _ => {}
    }
    out
}

/// Java `MotionEvent.getActionMasked()` constants. We can't reuse
/// `android_activity::input::MotionAction` because that enum's
/// constructor is private — `MotionEvent`s authored from arbitrary JNI
/// data only carry the integer.
const JAVA_ACTION_DOWN: i32 = 0;
const JAVA_ACTION_UP: i32 = 1;
const JAVA_ACTION_MOVE: i32 = 2;
const JAVA_ACTION_CANCEL: i32 = 3;
const JAVA_ACTION_POINTER_DOWN: i32 = 5;
const JAVA_ACTION_POINTER_UP: i32 = 6;
const JAVA_ACTION_HOVER_MOVE: i32 = 7;
const JAVA_ACTION_SCROLL: i32 = 8;

/// `MotionEvent.BUTTON_SECONDARY` — set when the user clicks the right
/// mouse button or does a two-finger tap on a touchpad.
const ANDROID_BUTTON_SECONDARY: i32 = 1 << 1;

/// Touch-translator for events arriving on extra `SurfaceView`s (i.e.
/// secondary gpui windows hosted by `multi_window`). The primary path uses
/// [`translate_motion_event`] which consumes android-activity's NDK-backed
/// `MotionEvent`; this one takes the raw fields we marshal across the JNI
/// boundary in `ExtraWindowActivity.forwardTouchEvent`.
///
/// Handles the same input vocabulary as the primary translator: touch
/// DOWN/MOVE/UP, mouse hover, mouse-wheel + trackpad two-finger scroll,
/// and physical secondary-button (right-click) on mouse / trackpad.
/// Multi-touch right-click synthesis (two-finger tap → secondary) is NOT
/// mirrored here — Settings / Keymap / Themes don't surface a context
/// menu, so the extra cost wouldn't pay off until we ship a window that
/// actually wants it.
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
    let modifiers = modifiers_from_meta(MetaState(meta_state as u32));
    let secondary_button = (button_state & ANDROID_BUTTON_SECONDARY) != 0;

    let mut out = Vec::new();
    match action_masked {
        JAVA_ACTION_DOWN | JAVA_ACTION_POINTER_DOWN => {
            // Right-click via mouse secondary button or trackpad two-
            // finger tap (Android resolves the gesture to BUTTON_SECONDARY
            // in button_state for us). Mirrors the primary translator at
            // events.rs:107-122.
            let button = if secondary_button {
                MouseButton::Right
            } else {
                MouseButton::Left
            };
            out.push(PlatformInput::MouseDown(MouseDownEvent {
                button,
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }));
        }
        JAVA_ACTION_UP | JAVA_ACTION_POINTER_UP | JAVA_ACTION_CANCEL => {
            // We don't track the latched-down button across events here —
            // for the Settings window's single-pointer-tap-or-secondary
            // workflow that's fine; emit Right-up if button_state still
            // shows secondary, else Left-up. End any multi-touch scroll.
            EXTRA_MULTI_TOUCH_CENTROID.with(|cell| cell.set(None));
            let button = if secondary_button {
                MouseButton::Right
            } else {
                MouseButton::Left
            };
            out.push(PlatformInput::MouseUp(MouseUpEvent {
                button,
                position,
                modifiers,
                click_count: 1,
            }));
        }
        JAVA_ACTION_MOVE => {
            // Multi-touch drag → synthesize scroll. Same Samsung-trackpad
            // story as the primary translator's Move arm; see the
            // `MULTI_TOUCH_CENTROID` doc.
            if positions.len() >= 2 {
                let cur = pointer_centroid(positions);
                let prev = EXTRA_MULTI_TOUCH_CENTROID.with(|cell| cell.replace(Some(cur)));
                if prev.is_none() {
                    // Cancel the latched Left from a single-finger Down
                    // that turned into a multi-finger drag.
                    out.push(PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count: 0,
                    }));
                }
                if let Some((lx, ly)) = prev {
                    let dx = cur.0 - lx;
                    let dy = cur.1 - ly;
                    if dx != 0.0 || dy != 0.0 {
                        out.push(PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position,
                            delta: ScrollDelta::Pixels(point(
                                px(dx / scale_factor),
                                px(dy / scale_factor),
                            )),
                            modifiers,
                            touch_phase: TouchPhase::Moved,
                        }));
                    }
                }
                return out;
            }
            EXTRA_MULTI_TOUCH_CENTROID.with(|cell| cell.set(None));
            let pressed = if secondary_button {
                Some(MouseButton::Right)
            } else {
                Some(MouseButton::Left)
            };
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: pressed,
                modifiers,
            }));
        }
        JAVA_ACTION_HOVER_MOVE => {
            // Mouse moved without a button held. The scrollbar autohide
            // state machine (`crates/ui/src/components/scrollbar.rs:1507`)
            // listens for `MouseMoveEvent { pressed_button: None }` to
            // fade the thumb in on parent-region entry.
            out.push(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }));
        }
        JAVA_ACTION_SCROLL => {
            // Mouse wheel + trackpad two-finger scroll. Android reports
            // +Vscroll for "wheel rotates away from user"; gpui expects
            // +Y to scroll content up (wheel-toward-user convention), so
            // negate Vscroll. Same fix the primary translator applies at
            // events.rs:266.
            if vscroll != 0.0 || hscroll != 0.0 {
                out.push(PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position,
                    delta: ScrollDelta::Lines(point(hscroll, -vscroll)),
                    modifiers,
                    touch_phase: TouchPhase::Moved,
                }));
            }
        }
        _ => {}
    }
    out
}

/// Same shape as [`translate_key_event`] but takes raw Android KeyEvent
/// fields instead of an `android_activity::KeyEvent` object. Used by the
/// ExtraWindowActivity JNI bridge (`multi_window::nativeOnExtraKeyEvent`)
/// which sees `MotionEvent`/`KeyEvent` via Java reflection — those Java
/// objects can't be reconstructed into `android_activity::KeyEvent` on
/// the Rust side, so we accept the primitive fields and rebuild the
/// translation pipeline on top of them.
///
/// `action`: `KeyEvent.ACTION_DOWN` (0) or `ACTION_UP` (1).
/// `keycode_raw`: Android `KeyEvent.getKeyCode()` (AKEYCODE_*).
/// `meta_state_raw`: Android `KeyEvent.getMetaState()` (META_* bitfield).
/// `repeat_count`: `KeyEvent.getRepeatCount()` for auto-repeat detection.
pub(crate) fn translate_extra_key_event(
    action: i32,
    keycode_raw: u32,
    meta_state_raw: u32,
    repeat_count: i32,
) -> Option<PlatformInput> {
    let meta = MetaState(meta_state_raw);
    let keycode = Keycode::from(keycode_raw);
    let modifiers = modifiers_from_meta(meta);

    if is_modifier_key(keycode) {
        return Some(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers,
            capslock: capslock_from_meta(meta),
        }));
    }

    let keystroke = build_keystroke(keycode, modifiers);

    // Android KeyEvent.ACTION_DOWN = 0, ACTION_UP = 1, ACTION_MULTIPLE = 2.
    // We follow the same translation policy as `translate_key_event`: only
    // Down/Up produce inputs; Multiple is reserved for synthesized soft-
    // keyboard char sequences we don't currently support.
    match action {
        0 => Some(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: repeat_count > 0,
            prefer_character_input: false,
        })),
        1 => Some(PlatformInput::KeyUp(KeyUpEvent { keystroke })),
        _ => None,
    }
}

fn modifiers_from_meta(meta: MetaState) -> Modifiers {
    Modifiers {
        shift: meta.shift_on(),
        control: meta.ctrl_on(),
        alt: meta.alt_on(),
        platform: meta.meta_on(),
        function: meta.function_on(),
    }
}

fn capslock_from_meta(meta: MetaState) -> Capslock {
    Capslock {
        on: meta.caps_lock_on(),
    }
}

fn is_modifier_key(code: Keycode) -> bool {
    use Keycode::*;
    matches!(
        code,
        ShiftLeft | ShiftRight | AltLeft | AltRight | CtrlLeft | CtrlRight | MetaLeft | MetaRight
    )
}

fn build_keystroke(code: Keycode, mut modifiers: Modifiers) -> Keystroke {
    let (key, key_char) = if let Some(named) = named_key(code) {
        // Space is the one named key where gpui still wants a printable
        // key_char so text-input paths can insert " ".
        let key_char = matches!(code, Keycode::Space).then(|| " ".to_string());
        (named.to_string(), key_char)
    } else if let Some(ch) = lowercased_key(code) {
        let key = ch.to_string();
        let typed = if modifiers.shift {
            apply_shift(ch)
        } else {
            ch
        };
        (key, Some(typed.to_string()))
    } else {
        (format!("{code:?}").to_lowercase(), None)
    };

    // Drop the shift modifier for non-alpha single-char keys — the shifted
    // value is already in `key_char` and bindings like `shift-1` should match
    // as `!`. Mirrors X11's `keystroke_from_xkb` behavior.
    if modifiers.shift
        && key.chars().count() == 1
        && key.chars().next().map_or(false, |c| {
            c.to_lowercase().to_string() == c.to_uppercase().to_string()
        })
    {
        modifiers.shift = false;
    }

    Keystroke {
        modifiers,
        key,
        key_char,
    }
}

fn named_key(code: Keycode) -> Option<&'static str> {
    use Keycode::*;
    Some(match code {
        Enter | NumpadEnter => "enter",
        Tab => "tab",
        Space => "space",
        Del => "backspace",
        ForwardDel => "delete",
        Escape => "escape",
        DpadUp => "up",
        DpadDown => "down",
        DpadLeft => "left",
        DpadRight => "right",
        MoveHome => "home",
        MoveEnd => "end",
        PageUp => "pageup",
        PageDown => "pagedown",
        Insert => "insert",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        _ => return None,
    })
}

fn lowercased_key(code: Keycode) -> Option<char> {
    use Keycode::*;
    Some(match code {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        Keycode0 => '0',
        Keycode1 => '1',
        Keycode2 => '2',
        Keycode3 => '3',
        Keycode4 => '4',
        Keycode5 => '5',
        Keycode6 => '6',
        Keycode7 => '7',
        Keycode8 => '8',
        Keycode9 => '9',
        Period => '.',
        Comma => ',',
        Slash => '/',
        Backslash => '\\',
        Semicolon => ';',
        Apostrophe => '\'',
        Grave => '`',
        Minus => '-',
        Equals => '=',
        LeftBracket => '[',
        RightBracket => ']',
        _ => return None,
    })
}

fn apply_shift(ch: char) -> char {
    match ch {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => ch.to_ascii_uppercase(),
    }
}
