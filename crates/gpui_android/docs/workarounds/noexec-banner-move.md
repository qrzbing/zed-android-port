# Noexec banner with confirm-dialog Move-to-local

**Status:** Active
**Phase / Commit:** L3d (initial), L9 (dialog + suppress)
**Files:** `crates/gpui_android/examples/zed_android/src/title_bar.rs`, `crates/gpui_android/src/storage.rs`

## Problem

When a user opens a project rooted on `/storage/emulated/0/...` (e.g. via
SAF picker), every build attempt fails with `EACCES` because the FUSE mount
is `noexec`. The user gets no UI signal that this will happen until the
build actually fails — at which point they've wasted compile time and have
no clear remediation path.

## Constraint

We can't make `/storage/emulated/0` exec-mounted. See
[android-noexec-mount.md](android-noexec-mount.md). The fix has to be at
the workflow layer: surface the constraint up-front and offer a one-click
escape.

## Solution

A yellow chip in the title bar, conditional on the worktree root being on
a noexec mount. Detection:

```rust
pub fn is_noexec_path(path: &Path) -> bool {
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path_c.as_ptr(), &mut buf) } != 0 {
        return false;
    }
    (buf.f_flag & libc::ST_NOEXEC) != 0
}
```

Render is gated on **both** the noexec check AND the per-path suppression
list — if either says skip, no banner. Click no longer copies straight
away; it pops a Trust-style three-button confirmation modal:

```rust
Button::new("zed-android-noexec-banner", "Builds won't run · Move")
    .on_click(cx.listener(move |_, _, window, cx| {
        let answer = window.prompt(
            PromptLevel::Warning,
            "Builds won't run on shared storage",
            Some(&detail),
            &["Copy to ~/projects", "Suppress this warning", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| match answer.await {
            Ok(0) => { cx.update(|cx| Self::start_move_to_local(path, cx)); }
            Ok(1) => {
                gpui_android::storage::add_noexec_suppressed(&path);
                this.update(cx, |_, cx| cx.notify()).ok();
            }
            _ => {}
        }).detach();
    }))
```

- **Copy** path is the L3d-era flow: resolve `~/projects/<basename>` with
  `-imported`/`-imported-N` suffixing, `cx.background_spawn`'d
  `copy_tree`, then `MultiWorkspace::open_project(...Activate)`. New
  worktree's root is exec-allowed, the banner re-checks `is_noexec_path`,
  finds it false, and doesn't render.
- **Suppress** writes the absolute path into
  `~/.cache/zed-android/noexec-suppressed.json` and notifies the
  TitleBar entity to redraw — banner vanishes for **this exact path**
  forever (until the user deletes the JSON or the path goes away).
  Per-path, not project-name-based, so import copies opened later still
  warn if they happen to land on a noexec mount.
- **Cancel** does nothing.

The suppress list is a `Vec<String>` of absolute paths serialized via
`serde_json` — same format used by other Zed preference files. Lives
under `~/.cache/zed-android/` rather than `~/.config/` because it's a
per-device cache (cross-device path duplication is unlikely and not
worth syncing).

## Why this works

- statvfs's `ST_NOEXEC` flag is the same kernel-level flag the exec syscall
  checks. If statvfs says noexec, exec WILL fail. If statvfs says exec-
  allowed, exec WILL succeed (assuming standard +x perms etc.). No false
  positives or negatives.
- Copy preserves file modes and symlinks; the imported tree is a faithful
  duplicate.
- `~/projects/` is on app-private storage, exec-allowed. So all build tools
  run natively from there.
- Original on /sdcard is left untouched — user can `git push`/`rsync`/
  whatever back to source if they want. We're a code editor, not a
  backup tool.

## Failure mode if regressed

- statvfs returns noexec=false on a path that's actually noexec → banner
  doesn't appear → user surprised by EACCES on first build. Mitigation: log
  the statvfs result if env var debug flag is set.
- Move action fails partway → partial copy at destination, original
  untouched. User can retry or `rm -rf` the partial.
- User has TERMUX__HOME unset (boot init failed) → both
  `start_move_to_local` and `add_noexec_suppressed` log an error and
  return without action. Banner re-shows next launch.
- Suppress JSON corrupted → `read_noexec_suppressed_list` returns empty
  (`serde_json::from_str` fails silently into `unwrap_or_default`); banner
  re-shows for all paths. User reapplies suppression.

## See also

- [android-noexec-mount.md](android-noexec-mount.md) — the underlying
  constraint
- [projects-workspace-import.md](projects-workspace-import.md) — the
  related "Import from sdcard" menu action that does the same copy
  proactively
- [trust-restore-from-db.md](trust-restore-from-db.md) — the imported copy
  is a different absolute path than the original; user re-trusts once,
  then it persists
