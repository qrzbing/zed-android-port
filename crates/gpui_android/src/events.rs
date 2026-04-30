use android_activity::input::{
    Axis, KeyAction, KeyEvent, Keycode, MetaState, MotionAction, MotionEvent,
};
use gpui::{
    Capslock, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollDelta, ScrollWheelEvent,
    TouchPhase, point, px,
};

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

/// Convert an Android `MotionEvent` (touch / mouse / stylus) into a gpui
/// `PlatformInput`. For MVP we treat the primary pointer as a left mouse —
/// pinch-zoom and other multi-touch gestures are skipped.
///
/// Coordinates from android come in physical pixels; gpui expects logical, so
/// we divide by the active scale_factor here.
pub(crate) fn translate_motion_event(
    event: &MotionEvent,
    scale_factor: f32,
) -> Option<PlatformInput> {
    if event.pointer_count() == 0 {
        return None;
    }
    let primary = event.pointer_at_index(0);
    let position = point(
        px(primary.x() / scale_factor),
        px(primary.y() / scale_factor),
    );
    let modifiers = modifiers_from_meta(event.meta_state());

    match event.action() {
        MotionAction::Down | MotionAction::PointerDown => {
            Some(PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
                first_mouse: true,
            }))
        }
        MotionAction::Up | MotionAction::PointerUp => Some(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        })),
        MotionAction::Move => Some(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers,
        })),
        MotionAction::HoverMove => Some(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        })),
        MotionAction::Cancel => Some(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 0,
        })),
        MotionAction::Scroll => {
            // Mouse wheel + trackpad two-finger scroll arrives as Vscroll/Hscroll
            // axes in `lines`. gpui's ScrollDelta::Lines uses the same unit, but
            // Android reports +Y for "up" (away from user); gpui expects +Y to
            // scroll content up (the wheel-rotates-toward-you convention), so we
            // negate Vscroll so trackpad-down scrolls content down.
            let vscroll = primary.axis_value(Axis::Vscroll);
            let hscroll = primary.axis_value(Axis::Hscroll);
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
