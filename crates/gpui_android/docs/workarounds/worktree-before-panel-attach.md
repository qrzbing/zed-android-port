# Create worktree before attaching project panel

**Status:** Active
**Phase / Commit:** `2642993bd9` — Create worktree before attaching project panel
**Files:** `crates/gpui_android/examples/zed_android/src/lib.rs` (the post-`open_window` spawn_in chain)

## Problem

Project panel didn't auto-open on launch even when a project was loaded
with files. User had to toggle it manually with `Ctrl+B` every session.

## Constraint

`ProjectPanel::starts_open()` returns `true` only when the project has at
least one worktree with a directory root. The dock-on-launch decision happens
inside `add_panel`. Previously we were doing:

```rust
window.spawn_in(cx, async move |...| {
    open_window(...).await;
    add_panel(project_panel, ...);   // worktree not yet created
    create_worktree(path).await;     // too late — starts_open() already returned false
});
```

`add_panel` ran before the worktree existed, `starts_open()` saw an empty
project, dock stayed closed. Then `create_worktree` finished and the
project gained a root, but nothing re-checked the auto-open condition.

## Solution

Schedule the worktree creation first, await its task inside the same
`spawn_in` that loads the panel. Same shape as production zed:

```rust
window.spawn_in(cx, async move |...| {
    open_window(...).await;
    create_worktree(path).await;     // worktree now exists
    add_panel(project_panel, ...);   // starts_open() sees it, opens dock
});
```

## Why this works

- The await on `create_worktree` ensures the worktree is registered with
  the project before `add_panel` queries it.
- `ProjectPanel::starts_open` reads the project's worktree list; with the
  await, that list is non-empty when queried.

## Failure mode if regressed

- Dock stays closed on launch despite having files. User has to manually
  toggle. Silent failure — no error, just missing UI.

## See also

- [multiworkspace-keymap-order.md](multiworkspace-keymap-order.md) — same
  spawn_in chain, related sequencing concerns
