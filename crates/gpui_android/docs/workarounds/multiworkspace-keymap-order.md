# MultiWorkspace wrapper + load keymap last

**Status:** Active
**Phase / Commit:** `701961be45` — Wrap Workspace in MultiWorkspace + load keymap last
**Files:** `crates/gpui_android/examples/zed_android/src/lib.rs` (boot order + workspace setup)

## Problem

The default keymap ships with Workspace-context bindings — `Ctrl+Alt+B` (toggle
left dock), `Ctrl+B`, `Ctrl+J` (toggle bottom dock), and many more — that
all silently failed to fire on Android. Pressing the key did nothing. No
panic, no log, just a no-op.

## Constraint

GPUI's action dispatcher matches keystrokes against bindings filtered by the
current `KeyContext` of the focused element tree. The default keymap gates
those bindings on `KeyContext("Workspace")`. Production zed establishes that
context inside `MultiWorkspace::render` via:

```rust
root.key_context(workspace.key_context(cx))
```

Our example was rendering `Workspace` directly as the window root, skipping
`MultiWorkspace`. So no Workspace KeyContext was ever set, no Workspace-gated
bindings ever matched, and the user's keypresses fell through to the no-op
default branch.

Separately: the keymap loader runs through registered actions to bind them
to keystrokes. Loading the keymap *before* every crate's `init` registered
its actions meant binding count was correct (loader doesn't filter by
availability) but the dispatch tree wasn't fully populated when bindings
hit the matcher. Production zed loads the keymap last for that reason.

## Solution

Two changes:

1. Wrap our window root in `MultiWorkspace`. The example's
   `cx.open_window` callback constructs `MultiWorkspace::new(workspace,
   cx)` and uses that as the window's root view. Now the Workspace
   KeyContext is established and `workspace.key_context(cx)` propagates
   through the dispatch tree.
2. Move `load_default_keymap` (or our equivalent) to the end of the boot
   chain, after every `init` call. Matches `crates/zed/src/main.rs` order.

## Why this works

- Every Workspace-gated binding now has a matching context in the focused
  element tree at dispatch time. `Ctrl+B` toggles the dock as designed.
- All action types are registered before the keymap matcher first runs,
  removing a class of "binding present but no handler" footguns.

## Failure mode if regressed

- Drop the `MultiWorkspace` wrap → every Workspace-gated keybinding
  silently fails. Symptom: keyboard shortcuts do nothing, no error in log.
  Hard to debug without remembering the KeyContext rule.
- Load keymap before init → most things still work because the loader is
  permissive, but new action types added later in the boot chain may not
  be bindable. Subtle.

## See also

- Production zed's main.rs:457 area for the canonical init order
- gpui's KeyContext + keymap matcher docs
