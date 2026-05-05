# Fire `on_active_status_change` after extra-window attach

**Status:** Active
**Phase / Commit:** L7 (post-shipping fix)
**Files:**
- `crates/gpui_android/src/window.rs` (`notify_active_status_change`)
- `crates/gpui_android/src/platform.rs` (`open_extra_window` deferred spawn)

## Problem

In the multi-Activity Settings window, the search field's text cursor
rendered **statically** on first open — visible but not blinking. After any
input event (tapping the search bar, typing), the cursor began blinking
normally and continued to behave correctly.

Production Zed on macOS/Linux: cursor blinks immediately on first paint.

## Constraint

The editor's cursor blink is gated on `BlinkManager::enabled`. `enable()`
gets called from two paths:

1. **`Editor::handle_focus`** — fires when the editor's `FocusHandle`
   becomes the window's focused handle (line `editor.rs:25994`).
2. **`cx.observe_window_activation`** — fires when the window's `active`
   state changes via the platform's `on_active_status_change` callback
   (line `editor.rs:2618`).

Settings UI explicitly focuses the search bar at construction
(`editor.focus_handle(cx).focus(window, cx)` at `settings_ui.rs:1696`),
so path (1) should fire. Empirically, **it does NOT** — instrumented
`BlinkManager::enable` never logs for the extra-window editor. Path (2)
is gpui's only other route, and it requires the platform to call
`on_active_status_change` after the gpui Window registers its callback.

We never invoked that callback for any window — primary GameActivity got
away with it because the welcome page has no focused text input that
visibly cares; Settings exposed the bug.

Critical timing detail: gpui registers the platform callback inside
`Window::new`, which runs AFTER our `Platform::open_window` returns.
So we can't fire it synchronously from `open_extra_window` — the slot is
empty at that point.

## Solution

Add a `notify_active_status_change(active)` method on
`AndroidWindowStatePtr` that invokes the registered callback if present,
and schedule a **deferred** call via the foreground executor in
`open_extra_window` so it lands on a tick AFTER gpui's `Window::new` has
finished registering callbacks:

```rust
// In AndroidWindowStatePtr (window.rs):
pub(crate) fn notify_active_status_change(&self, active: bool) {
    let callback = self.callbacks.borrow_mut().active_status_change.take();
    if let Some(mut callback) = callback {
        callback(active);
        self.callbacks.borrow_mut().active_status_change = Some(callback);
    }
}

// In open_extra_window (platform.rs), at the end:
let executor = self.common.borrow().foreground_executor.clone();
let window_ptr = window.ptr();
executor
    .spawn(async move {
        window_ptr.notify_active_status_change(true);
    })
    .detach();
```

The deferred spawn runs on the next foreground tick, after gpui's
`Window::new` has wired all callbacks. The callback fires
`window.activation_observers`, which the editor registered via
`cx.observe_window_activation`. The observer reads
`window.is_window_active()` (true) and calls `BlinkManager::enable`. Blink
loop starts. Cursor animates.

## Why this works

Verified end-to-end on device. Instrumented `BlinkManager::enable`
confirms: before the fix, `enable()` was never called for the Settings
search bar editor. After the fix, `enable() called, was_enabled=false`
fires within ~500ms of window open, and the blink loop runs cleanly with
epoch counter incrementing every 500ms.

Without the deferred spawn (firing synchronously inside
`open_extra_window`): `state.callbacks.active_status_change` is `None`
because gpui hasn't run `Window::new` yet — the callback is registered at
gpui's `window.rs:1455`, which is part of the post-`open_window` Window
construction flow.

## Failure mode if regressed

- Settings search cursor renders statically (visible but not blinking)
  on first open. After any input the bug "self-heals" because
  `Editor::handle_input → update_selections → pause_blinking` triggers
  the blink loop manually — but only after `enable()` has run, which it
  hadn't until our notify path.
- Removing the `executor.spawn(...)` and firing synchronously: `enable()`
  never runs because the callback slot is empty at our call site.
- Forgetting to fire entirely (revert): same as the original bug.

## Why not also wire this for primary

Primary GameActivity could plausibly have the same issue, but in practice
the welcome page has no focus-on-init text input — there's nothing to
visibly miscarry. Wiring `on_active_status_change` for primary would
require bridging GameActivity's `onWindowFocusChanged` JNI events into our
NDK loop, which is more work for a less observable benefit. Defer until
something user-facing demands it.

## See also

- [Multi-Activity OS-chromed extra windows](multi-activity-os-chrome.md)
- `editor::blink_manager::BlinkManager` — the blink state machine
- `gpui::Window::new` (`gpui/src/window.rs:1455`) — where the callback is registered
- `gpui::App::observe_window_activation` (`gpui/src/app/context.rs:444`) — observer registration
