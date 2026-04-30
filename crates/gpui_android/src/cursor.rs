//! Bridge between gpui's `CursorStyle` and Android's `PointerIcon`.
//!
//! On platforms with a hardware pointer (BT mouse, attached trackpad) the
//! tablet decides which cursor to draw via `View.setPointerIcon`. The view
//! lives in the JVM so this module hops through JNI to set it on the
//! activity's decor view whenever gpui asks for a cursor change.

use std::cell::Cell;

use android_activity::AndroidApp;
use gpui::CursorStyle;
use jni::{JavaVM, objects::JObject, sys::jint};

thread_local! {
    static LAST_STYLE: Cell<Option<CursorStyle>> = const { Cell::new(None) };
}

/// Map a gpui `CursorStyle` to one of Android's `PointerIcon.TYPE_*`
/// constants. Values come from
/// `frameworks/base/core/java/android/view/PointerIcon.java`.
fn pointer_icon_type(style: CursorStyle) -> jint {
    match style {
        CursorStyle::Arrow => 1000,                    // TYPE_DEFAULT
        CursorStyle::IBeam => 1008,                    // TYPE_TEXT
        CursorStyle::IBeamCursorForVerticalLayout => 1009, // TYPE_VERTICAL_TEXT
        CursorStyle::Crosshair => 1007,                // TYPE_CROSSHAIR
        CursorStyle::ClosedHand => 1021,               // TYPE_GRABBING
        CursorStyle::OpenHand => 1020,                 // TYPE_GRAB
        CursorStyle::PointingHand => 1002,             // TYPE_HAND
        CursorStyle::ResizeLeft
        | CursorStyle::ResizeRight
        | CursorStyle::ResizeLeftRight
        | CursorStyle::ResizeColumn => 1014,           // TYPE_HORIZONTAL_DOUBLE_ARROW
        CursorStyle::ResizeUp
        | CursorStyle::ResizeDown
        | CursorStyle::ResizeUpDown
        | CursorStyle::ResizeRow => 1015,              // TYPE_VERTICAL_DOUBLE_ARROW
        CursorStyle::ResizeUpRightDownLeft => 1017,    // TYPE_TOP_RIGHT_DIAGONAL_DOUBLE_ARROW
        CursorStyle::ResizeUpLeftDownRight => 1016,    // TYPE_TOP_LEFT_DIAGONAL_DOUBLE_ARROW
        CursorStyle::OperationNotAllowed => 1012,      // TYPE_NO_DROP
        CursorStyle::DragLink => 1010,                 // TYPE_ALIAS
        CursorStyle::DragCopy => 1011,                 // TYPE_COPY
        CursorStyle::ContextualMenu => 1001,           // TYPE_CONTEXT_MENU
        CursorStyle::None => 0,                        // TYPE_NULL (hidden)
    }
}

/// Set the system pointer icon on the activity's decor view via JNI.
/// Call from the main thread; android-activity's `android_main` runs on
/// the Android UI thread so this is safe in our normal event flow.
///
/// gpui's `reset_cursor_style` fires on every input event, so we cache the
/// last style and skip the JNI round-trip when nothing changed.
pub(crate) fn set_pointer_icon(android_app: &AndroidApp, style: CursorStyle) {
    let unchanged = LAST_STYLE.with(|cell| cell.get() == Some(style));
    if unchanged {
        return;
    }
    if let Err(err) = set_pointer_icon_inner(android_app, style) {
        log::warn!("set_pointer_icon({style:?}) failed: {err:#}");
        return;
    }
    LAST_STYLE.with(|cell| cell.set(Some(style)));
}

fn set_pointer_icon_inner(
    android_app: &AndroidApp,
    style: CursorStyle,
) -> anyhow::Result<()> {
    let icon_type = pointer_icon_type(style);
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast())? };
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as _) };

    let window = env
        .call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])?
        .l()?;
    let decor = env
        .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
        .l()?;
    let pointer_icon_class = env.find_class("android/view/PointerIcon")?;
    let pointer_icon = env
        .call_static_method(
            &pointer_icon_class,
            "getSystemIcon",
            "(Landroid/content/Context;I)Landroid/view/PointerIcon;",
            &[(&activity).into(), icon_type.into()],
        )?
        .l()?;
    env.call_method(
        &decor,
        "setPointerIcon",
        "(Landroid/view/PointerIcon;)V",
        &[(&pointer_icon).into()],
    )?;
    Ok(())
}
