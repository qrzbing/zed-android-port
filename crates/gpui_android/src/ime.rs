//! IME / soft-keyboard bridge.
//!
//! Android IMEs (Gboard / Swiftkey / Samsung / etc.) talk to apps via
//! `InputConnection`. For typical `EditText`-hosted apps the framework
//! provides everything; for NDK / native-renderer apps (Zed) we host
//! our own. The Kotlin side (`ZdroidInputConnection.kt`,
//! `ImeHostView.kt`) subclasses `BaseInputConnection`; each IME
//! callback fires a JNI call into this module.
//!
//! Threading: JNI callbacks arrive on Android's UI thread.
//! `AndroidWindowState` (and its `PlatformInputHandler`) live in
//! `Rc<RefCell<_>>` on the game thread — not Send. So we marshal
//! every IME event onto an mpsc channel and the game thread drains it
//! each iteration of the main loop (same pattern as `multi_window`
//! and `captured_pointer`). Show/hide IME goes the other direction —
//! Rust → Kotlin — via direct JNI calls on `MainActivity.showIme()` /
//! `hideIme()`.

use std::sync::Mutex;

use anyhow::Context as _;
use android_activity::AndroidApp;
use futures::channel::mpsc;
use jni::JNIEnv;
use jni::JavaVM;
use jni::objects::{JObject, JString};

use crate::window::AndroidWindowStatePtr;

/// Input events the IME wants the gpui side to apply. Each variant
/// corresponds to one `InputConnection` method and translates to one
/// `PlatformInputHandler` call (or hardware-key path for sendKeyEvent).
pub(crate) enum ImeEvent {
    /// `commitText(text, newCursorPosition)`. Final text the IME wants
    /// inserted at the cursor (or replacing any active composition).
    CommitText { text: String, new_cursor_position: i32 },
    /// `setComposingText(text, newCursorPosition)`. In-progress
    /// composition (CJK, gesture typing, prediction).
    SetComposingText { text: String, new_cursor_position: i32 },
    /// `finishComposingText()`. End of composition without further
    /// edits.
    FinishComposingText,
    /// `deleteSurroundingText(before, after)`. Backspace / delete span
    /// around the cursor.
    DeleteSurroundingText { before_length: i32, after_length: i32 },
    /// `sendKeyEvent(KeyEvent)`. Hardware-style key delivery from the
    /// IME (Enter, arrows, etc.). Routed through the existing
    /// `events::keyboard::translate_extra_key_event` translator.
    KeyEvent {
        action: i32,
        keycode: u32,
        meta_state: u32,
        repeat_count: i32,
    },
    /// `performEditorAction(actionId)` — Enter / Done / Next / Search.
    /// Not handled yet; logged for now.
    EditorAction { action_id: i32 },
}

static EVENT_TX: Mutex<Option<mpsc::UnboundedSender<ImeEvent>>> = Mutex::new(None);

/// Construct a fresh sender/receiver pair. Returns the receiver for
/// the platform to drain. Safe to call multiple times (Activity-
/// recreation idempotent); each call drops the previous sender.
pub(crate) fn init_event_channel() -> mpsc::UnboundedReceiver<ImeEvent> {
    let (tx, rx) = mpsc::unbounded();
    *EVENT_TX.lock().unwrap() = Some(tx);
    rx
}

fn dispatch_event(event: ImeEvent) {
    let guard = EVENT_TX.lock().unwrap();
    let Some(tx) = guard.as_ref() else {
        log::warn!("ime: event arrived before init_event_channel");
        return;
    };
    if let Err(err) = tx.unbounded_send(event) {
        log::warn!("ime: dispatch_event failed: {err:#}");
    }
}

/// Drain pending IME events into the primary window's input handler.
/// Called once per platform loop iteration on the game thread.
pub(crate) fn drain_ime_events(
    window_ptr: &AndroidWindowStatePtr,
    rx: &mut mpsc::UnboundedReceiver<ImeEvent>,
) {
    while let Ok(Some(event)) = rx.try_next() {
        apply_event(window_ptr, event);
    }
}

fn apply_event(window_ptr: &AndroidWindowStatePtr, event: ImeEvent) {
    match event {
        ImeEvent::CommitText { text, .. } => {
            let mut state = window_ptr.state.borrow_mut();
            if let Some(handler) = state.input_handler.as_mut() {
                handler.replace_text_in_range(None, &text);
            } else {
                log::debug!("ime::commit_text dropped (no input handler): {text:?}");
            }
        }
        ImeEvent::SetComposingText { text, .. } => {
            let mut state = window_ptr.state.borrow_mut();
            if let Some(handler) = state.input_handler.as_mut() {
                handler.replace_and_mark_text_in_range(None, &text, None);
            }
        }
        ImeEvent::FinishComposingText => {
            let mut state = window_ptr.state.borrow_mut();
            if let Some(handler) = state.input_handler.as_mut() {
                handler.unmark_text();
            }
        }
        ImeEvent::DeleteSurroundingText {
            before_length,
            after_length,
        } => {
            let mut state = window_ptr.state.borrow_mut();
            let Some(handler) = state.input_handler.as_mut() else {
                return;
            };
            let Some(selection) = handler.selected_text_range(false) else {
                return;
            };
            let cursor = selection.range.start;
            let before = (before_length.max(0) as usize).min(cursor);
            let after = after_length.max(0) as usize;
            let start = cursor - before;
            let end = selection.range.end + after;
            handler.replace_text_in_range(Some(start..end), "");
        }
        ImeEvent::KeyEvent {
            action,
            keycode,
            meta_state,
            repeat_count,
        } => {
            // Route through the same translator the extra-window key
            // path uses (`translate_extra_key_event`) so the gpui side
            // receives the same `PlatformInput::KeyDown` / `KeyUp` it
            // would from a hardware keyboard.
            if let Some(input) =
                crate::events::translate_extra_key_event(action, keycode, meta_state, repeat_count)
            {
                window_ptr.handle_input(input);
            }
        }
        ImeEvent::EditorAction { action_id } => {
            log::info!("ime::editor_action {action_id} (not yet routed)");
        }
    }
}

/// Show the soft keyboard. Calls `MainActivity.showIme()` on the
/// activity backing this app, which requests focus on `ImeHostView`
/// and invokes `InputMethodManager.showSoftInput`. Idempotent —
/// repeated calls when the IME is already up are no-ops on the OS
/// side. Logs but does not panic on failure (Activity recreation
/// in flight, etc.).
pub(crate) fn show_keyboard(android_app: &AndroidApp) {
    if let Err(err) = call_activity_void(android_app, "showIme") {
        log::warn!("ime::show_keyboard failed: {err:#}");
    }
}

/// Hide the soft keyboard. Pairs with `show_keyboard`.
pub(crate) fn hide_keyboard(android_app: &AndroidApp) {
    if let Err(err) = call_activity_void(android_app, "hideIme") {
        log::warn!("ime::hide_keyboard failed: {err:#}");
    }
}

fn call_activity_void(android_app: &AndroidApp, method: &str) -> anyhow::Result<()> {
    let vm_ptr = android_app.vm_as_ptr();
    let activity_ptr = android_app.activity_as_ptr();
    if vm_ptr.is_null() || activity_ptr.is_null() {
        anyhow::bail!("AndroidApp vm/activity pointer is null");
    }
    let vm = unsafe { JavaVM::from_raw(vm_ptr as _) }.context("JavaVM::from_raw")?;
    let mut env = vm.attach_current_thread().context("attach_current_thread")?;
    let activity = unsafe { JObject::from_raw(activity_ptr as _) };
    env.call_method(&activity, method, "()V", &[])
        .with_context(|| format!("call MainActivity.{method}()"))?;
    Ok(())
}

// --------------------------------------------------------------------
// JNI entry points called from `ZdroidInputConnection` on the UI
// thread. Each pushes an `ImeEvent` onto the channel; the game thread
// drains.
// --------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeCommitText<'local>(
    mut env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    text: JString<'local>,
    new_cursor_position: i32,
) {
    let text: String = env.get_string(&text).map(|s| s.into()).unwrap_or_default();
    dispatch_event(ImeEvent::CommitText {
        text,
        new_cursor_position,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeSetComposingText<'local>(
    mut env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    text: JString<'local>,
    new_cursor_position: i32,
) {
    let text: String = env.get_string(&text).map(|s| s.into()).unwrap_or_default();
    dispatch_event(ImeEvent::SetComposingText {
        text,
        new_cursor_position,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeFinishComposingText<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
) {
    dispatch_event(ImeEvent::FinishComposingText);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeDeleteSurroundingText<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    before_length: i32,
    after_length: i32,
) {
    dispatch_event(ImeEvent::DeleteSurroundingText {
        before_length,
        after_length,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeSendKeyEvent<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    action: i32,
    keycode: i32,
    meta_state: i32,
    repeat_count: i32,
) {
    dispatch_event(ImeEvent::KeyEvent {
        action,
        keycode: keycode as u32,
        meta_state: meta_state as u32,
        repeat_count,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImePerformEditorAction<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    action_id: i32,
) {
    dispatch_event(ImeEvent::EditorAction { action_id });
}
