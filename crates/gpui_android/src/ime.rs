//! IME / soft-keyboard bridge.
//!
//! The Android IME (Gboard / Swiftkey / Samsung / etc.) talks to apps
//! via `InputConnection`. For typical `EditText`-hosted apps the
//! framework provides everything; for NDK / native-renderer apps
//! (Zed) we host our own. The Kotlin side
//! (`ZdroidInputConnection.kt`, `ImeHostView.kt`) subclasses
//! `BaseInputConnection`; each IME callback fires a JNI call into
//! this module, which dispatches into gpui's `PlatformInputHandler`
//! (the same trait macOS `NSTextInputClient` and Linux
//! `text-input-v3` wire to).
//!
//! Phase 0 (this commit): stub JNI entry points that log only. No
//! actual handler dispatch yet — just verifying the symbol set
//! resolves at app startup so the Kotlin `external fun` declarations
//! in `NativeBridge` don't blow up on first call. Subsequent phases
//! wire each method to `PlatformInputHandler`.

use jni::JNIEnv;
use jni::objects::{JObject, JString};

/// IME wants to commit final text at the current cursor (replacing
/// any active composition). Phase 0: log only.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeCommitText<'local>(
    mut env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    text: JString<'local>,
    new_cursor_position: i32,
) {
    let text: String = env
        .get_string(&text)
        .map(|s| s.into())
        .unwrap_or_default();
    log::info!("ime::commit_text text={text:?} cursor={new_cursor_position}");
}

/// IME wants to set in-progress composition (CJK, gesture typing,
/// prediction). Phase 0: log only.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeSetComposingText<'local>(
    mut env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    text: JString<'local>,
    new_cursor_position: i32,
) {
    let text: String = env
        .get_string(&text)
        .map(|s| s.into())
        .unwrap_or_default();
    log::info!("ime::set_composing_text text={text:?} cursor={new_cursor_position}");
}

/// IME finished composing without further edits. Phase 0: log only.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeFinishComposingText<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
) {
    log::info!("ime::finish_composing_text");
}

/// IME wants to delete N characters before the cursor and M after.
/// Phase 0: log only.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeDeleteSurroundingText<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    before_length: i32,
    after_length: i32,
) {
    log::info!("ime::delete_surrounding_text before={before_length} after={after_length}");
}

/// IME wants to deliver a hardware-style key event (Enter, Backspace,
/// arrows, etc.). Phase 0: log only. Phase 1 routes through the
/// existing `events::keyboard` translator.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImeSendKeyEvent<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    action: i32,
    keycode: i32,
    meta_state: i32,
    repeat_count: i32,
) {
    log::info!(
        "ime::send_key_event action={action} keycode={keycode} meta={meta_state:#x} \
         repeat={repeat_count}"
    );
}

/// IME `performEditorAction` — Enter / Done / Next / Search. Phase 0:
/// log only. Phase 1+ emits a gpui action.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeImePerformEditorAction<'local>(
    _env: JNIEnv<'local>,
    _bridge: JObject<'local>,
    action_id: i32,
) {
    log::info!("ime::perform_editor_action action_id={action_id}");
}
