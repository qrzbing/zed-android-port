# `with_active_or_new_workspace` falls back to existing Workspace on Android

**Status:** Active
**Phase / Commit:** L7g (post-L7 ship hotfix)
**Files:** `crates/workspace/src/workspace.rs` (`with_active_or_new_workspace`)

## Problem

User-reported state machine: open Settings (an extra OS-chromed Activity).
Then open Zed → Settings → Select Theme via the menu. Instead of the theme
picker appearing as a modal inside the existing Workspace, **a brand new
freeform Activity spawns showing a duplicate Workspace** with the theme
picker open inside it. Tapping inside that duplicate Workspace crashed.

After dismissing the duplicate + Settings, opening the theme picker again
"did nothing" — actually it spawned yet another duplicate Workspace in the
background that the user couldn't see. Same pattern for command palette,
recent projects picker, keymap editor, settings profile selector, devcontainer
launcher, etc. — anything routed via `with_active_or_new_workspace`.

## Constraint

`workspace.rs:10790` currently does:

```rust
pub fn with_active_or_new_workspace(cx: &mut App, f: impl ...) {
    match cx.active_window().and_then(|w| w.downcast::<MultiWorkspace>()) {
        Some(multi_workspace) => { /* defer modal toggle */ }
        None => { /* open_new → cx.open_window for a fresh Workspace */ }
    }
}
```

On macOS / Linux / Windows this is fine — windows are cheap, "no active
workspace → spawn one" is a sensible default. On Android with the L7
multi-Activity setup, `cx.active_window()` can be the Settings extra
window's handle, whose root view is `SettingsWindow`, not `MultiWorkspace`.
The downcast fails → falls into `open_new` → `cx.open_window` for a fresh
Workspace → our `AndroidPlatform::open_window` routes the secondary call
into `open_extra_window` → spawns another `ExtraWindowActivity` → user
sees a duplicate Zed in OS chrome.

We always have **exactly one** Workspace alive on Android (the primary
GameActivity-hosted one). The desktop notion of "if active isn't a
workspace, the user wants a new one" is wrong here — they always want the
existing primary.

## Solution

Android-only fallback: when active_window isn't a `MultiWorkspace`, scan
all open windows for one that is. Only fall through to `open_new` if no
`MultiWorkspace` exists at all (genuine "no workspace anywhere" case).

```rust
let active = cx
    .active_window()
    .and_then(|w| w.downcast::<MultiWorkspace>());
let target = if cfg!(target_os = "android") {
    active.or_else(|| {
        cx.windows()
            .into_iter()
            .find_map(|w| w.downcast::<MultiWorkspace>())
    })
} else {
    active
};
match target {
    Some(multi_workspace) => { /* unchanged: defer modal toggle */ }
    None => { /* unchanged: open_new */ }
}
```

`cfg!(target_os = "android")` guards the new branch so desktop semantics
are unchanged (`active.or_else(...)` is unevaluated, optimized out).

## Why this works

On Android there's exactly one MultiWorkspace and it's always the
primary. Routing modals to it regardless of which extra Activity is
currently focused is the right behavior — that primary IS where the
user's project state lives.

Verified on device: with Settings extra Activity in foreground, opening
Select Theme via primary's Zed menu now toggles the theme picker as a
modal inside the primary Workspace. No new ExtraWindowActivity spawns,
no crash, no zombie windows.

The crash on tap inside the duplicate Workspace was a side-effect of two
`MultiWorkspace` instances trying to share singleton state (project,
fs, etc.) — by not creating the duplicate at all, the crash path is
eliminated entirely.

## Failure mode if regressed

- Open Settings as an extra Activity → tap a `with_active_or_new_workspace`
  action (theme picker, command palette, recent projects, keymap editor)
  → second freeform window spawns showing a duplicate Workspace. Tapping
  in it crashes.
- After both windows close, the next attempt to open such a modal
  silently no-ops because a stale duplicate Workspace is alive in the
  background.

## See also

- [Multi-Activity OS-chromed extra windows](multi-activity-os-chrome.md)
- `theme_selector::init` (`crates/theme_selector/src/theme_selector.rs:31`) — uses `with_active_or_new_workspace` for the picker toggle, the original repro path
- Other `with_active_or_new_workspace` callers (all benefit from this fix on Android): `recent_projects`, `keymap_editor`, `settings_profile_selector`, `dev_container`, `zed::main`, etc.
