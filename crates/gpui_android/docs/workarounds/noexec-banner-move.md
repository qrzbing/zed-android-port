# Noexec banner with one-tap Move-to-local

**Status:** Active
**Phase / Commit:** L3d
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

Render:

```rust
Button::new("zed-android-noexec-banner", "Builds won't run · Move")
    .style(ButtonStyle::Tinted(TintColor::Warning))
    .start_icon(Icon::new(IconName::Warning))
    .tooltip(/* explanation */)
    .on_click(move |_, _, cx| Self::start_move_to_local(click_path.clone(), cx))
```

Click handler in `start_move_to_local`:

1. Resolve `~/projects/<basename>` (with `-imported`/`-imported-N`
   suffixing if it already exists).
2. `cx.background_spawn` runs `gpui_android::storage::copy_tree(src, dst)`.
3. On copy success, `MultiWorkspace::open_project(vec![dst], Activate, ...)`.
4. The new project opens, the noexec check on its root returns false, the
   banner doesn't render. User can build.

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
- User has TERMUX__HOME unset (boot init failed) → start_move_to_local logs
  error and returns without action.

## See also

- [android-noexec-mount.md](android-noexec-mount.md) — the underlying
  constraint
- [projects-workspace-import.md](projects-workspace-import.md) — the
  related "Import from sdcard" menu action that does the same copy
  proactively
- [trust-restore-from-db.md](trust-restore-from-db.md) — the imported copy
  is a different absolute path than the original; user re-trusts once,
  then it persists
