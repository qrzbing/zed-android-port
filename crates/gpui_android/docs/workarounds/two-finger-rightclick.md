# Two-finger tap → right click

**Status:** Active

Touchscreens have no right mouse button. crates/gpui_android/src/window.rs detects two-finger tap or long-press at MotionEvent::Up boundary, cancels the buffered left-click, fires synthetic MouseDown(Right) + MouseUp(Right). Project panel context menu and other on_secondary_mouse_down handlers fire correctly.

**Detailed writeup: TODO.** Stub created so the index links resolve.
