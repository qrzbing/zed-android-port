# Storage Access Framework picker integration

**Status:** Active
**Phase / Commit:** `0caf3f20b9` — Wire Android Storage Access Framework to gpui's path prompts
**Files:** `crates/gpui_android/src/saf.rs`, `crates/gpui_android/examples/zed_android/android/app/src/main/kotlin/.../MainActivity.kt`

## Problem

gpui's `prompt_for_paths` and `prompt_for_new_path` traditionally pop a
native file dialog (NSOpenPanel on macOS, GTK FileChooser on Linux). On
Android there's no equivalent — the system path is the Storage Access
Framework (SAF), which fires `Intent`s like `ACTION_OPEN_DOCUMENT_TREE` and
returns `content://` URIs that don't map to filesystem paths directly.

## Constraints

Several Android quirks bite this flow:

1. **`ActivityResultLauncher` silently no-ops** when `launch()` is called
   from a non-STARTED lifecycle. The typical state when the call comes from
   gpui's render thread (which can be in any lifecycle state) is non-STARTED,
   so the modern API just doesn't fire. AGDK's own samples sidestep this by
   using the legacy `startActivityForResult` + `onActivityResult` path.
2. **`content://` URIs aren't filesystem paths.** RealFs, worktrees, project
   panel — every part of zed downstream of the picker assumes a POSIX path.
3. **Second `cx.open_window` panics with `ERROR_NATIVE_WINDOW_IN_USE_KHR`** —
   `workspace::open_paths` opens a new window per project on desktop;
   Android only has one window. The platform's `open_window` returning Err
   on the second call is what keeps the editor alive.

## Solution

Three pieces:

1. **JNI bridge** (`saf.rs`) holds a `Mutex<Option<Pending>>` slot. When
   gpui calls `prompt_for_paths`, we stash the oneshot sender and call
   `MainActivity.launchOpenTree()` via JNI.
2. **Legacy `startActivityForResult` path** in MainActivity.kt (not
   ActivityResultLauncher). Picked URI returned via
   `Java_dev_zed_zed_android_MainActivity_onPickerResult` JNI callback,
   which decodes to a POSIX path under `/storage/emulated/0` and resolves
   the stashed oneshot.
3. **`open_window` returns Err on second call** so `workspace::open_paths`
   sees a "no slot available" failure instead of panicking. The
   example's `Open` action handler catches this and re-routes to
   `MultiWorkspace::open_project` which adds the path as a worktree on the
   existing window.

## Why this works

- Tree URIs from primary external storage have a predictable form
  (`content://com.android.externalstorage.documents/tree/primary%3A<percent-encoded-path>`)
  that decodes to a `/storage/emulated/0/<path>` POSIX path Android also
  exposes through normal filesystem syscalls.
- Legacy `startActivityForResult` works regardless of lifecycle state
  because it's a direct intent dispatch, not a state-machine-gated callback.
- Single-window assumption matches Android's task model — opening
  additional projects becomes worktree additions, not new windows.

## Failure mode if regressed

- ActivityResultLauncher reintroduced → first SAF pick after backgrounding
  silently no-ops, no error UI, user is confused.
- Non-primary-volume URIs (e.g. SD card adopted storage with UUID volume
  IDs) hit the path decoder and silently fall through. Users see "couldn't
  find path" with a `content://` URI in the error. Mitigation documented;
  fix is to extend the decoder for non-primary volumes when needed.

## See also

- [projects-workspace-import.md](projects-workspace-import.md) — what
  happens after a path is picked
- [storage-permission-jni.md](storage-permission-jni.md) — runtime perms
  for direct RealFs reads after the SAF pick
