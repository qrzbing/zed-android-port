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
            // the normal Up(Left).
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
        }
        MotionAction::Move => {
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
