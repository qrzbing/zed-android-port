# Zed on Android

Real [Zed](https://zed.dev) — workspace, editor, project panel, command
palette, vim mode, settings UI — running natively on an Android tablet
through a custom `gpui_android` platform backend.

This is an **experiment**, not a product. It is not affiliated with or
endorsed by Zed Industries. It exists because nothing was stopping it
from existing — gpui is platform-agnostic in shape, Android has Vulkan
and a JVM, Rust speaks JNI, and the rest is plumbing. A lot of plumbing.

![Workspace running on Tab S9 Ultra (placeholder)](docs/screenshots/workspace.png)

## Status

| Area                                       | State |
| ------------------------------------------ | ----- |
| Vulkan rendering through wgpu              | ✓     |
| Workspace shell (tabs, status bar, dock)   | ✓     |
| Project panel + worktrees                  | ✓     |
| Command palette + buffer / project search  | ✓     |
| Vim mode + settings UI                     | ✓     |
| One Dark / One Light from system theme     | ✓     |
| Hardware keyboard via Bluetooth            | ✓     |
| Trackpad / mouse + cursor change on hover  | ✓     |
| Two-finger / wheel scroll                  | ✓     |
| Long-press → context menu (touch)          | ✓     |
| Storage Access Framework folder picker     | ✓     |
| Soft keyboard / IME bridge                 | pending |
| Choreographer-aligned vsync, Turnip driver | pending |
| LSP servers running on-device              | out of scope |
| Multi-window                               | won't fix (one Android window) |

![Welcome page (placeholder)](docs/screenshots/welcome.png)
![Project panel + editor (placeholder)](docs/screenshots/project_panel.png)
![Command palette (placeholder)](docs/screenshots/command_palette.png)

## Why

Tablets with keyboards are real laptops now. Samsung's Tab S9 Ultra has
a Snapdragon 8 Gen 2, 12 GB of RAM, and a Vulkan-capable Adreno 740. The
hardware comfortably runs a real text editor; the only thing missing is
software that treats it like one. Termux + an SSH tunnel covers part of
the gap, but the editing experience is still a terminal vim/emacs over
a network. Zed compiles for `aarch64-linux-android` if you let it, so
this experiment puts the actual editor on the tablet.

It also tests how portable gpui really is. The answer turned out to be
"very" — the Editor element runs unchanged. The work was at the platform
boundary: a `Platform` trait impl, a Vulkan surface lifecycle that
matches Android's `ANativeWindow` events, JNI for cursor + SAF, and
working around half a dozen "this assumes a desktop file system" calls
inside Zed itself.

## Hardware tested

The port is hardware-verified on a **Samsung Galaxy Tab S9 Ultra**
(Snapdragon 8 Gen 2 / Adreno 740, Android 14 / One UI 6). It should run
on any Android 8+ device with Vulkan 1.1, but nothing else has been
tried. Adreno is the only driver that's been exercised — Mali / Xclipse
will compile but may need shader tweaks.

## Build

You'll need:

- Rust toolchain with the `aarch64-linux-android` target
  (`rustup target add aarch64-linux-android`)
- [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk)
  (`cargo install cargo-ndk`)
- Android NDK r27 (`sdkmanager "ndk;27.0.12077973"` via
  `android-commandlinetools`)
- Gradle 8+
- `adb` on `$PATH`

A device with developer options + USB debugging enabled, plugged in.

```sh
cd crates/gpui_android/examples/zed_android

# 1. Build the .so. NDK platform 26+ is required (cpal links libaaudio,
#    which only ships in API 26 sysroots), and `+fp16` enables the SIMD
#    path that gemm-f16 (transitively pulled in via candle) needs on
#    Snapdragon-class CPUs. The .cargo/config.toml in this directory
#    already sets the target feature; you only need to point at the NDK.
ANDROID_NDK_HOME=/path/to/ndk/27.0.12077973 \
  cargo ndk --target arm64-v8a --platform 26 build

# 2. Stage the freshly built library where Gradle expects it.
cp target/aarch64-linux-android/debug/libzed_android.so \
   android/app/src/main/jniLibs/arm64-v8a/libzed_android.so

# 3. Build and install the APK.
cd android
gradle assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.zdroid/.MainActivity
```

The first build takes ~10 minutes. Incremental Rust rebuilds are
~10–20 s; Gradle re-pack + install is another few seconds.

A logcat session that captures everything Zed-side (boot init, panel
attach, JNI calls, panics) is:

```sh
adb logcat -G 16M       # bump the ring buffer once per device session
adb logcat -d | grep -E "zed_android|saf:|RustPanic"
```

## Architecture

- `crates/gpui_android` — `gpui::Platform` impl. Owns the run loop,
  Vulkan surface lifecycle, dispatcher, JNI helpers (cursor mapping,
  SAF picker), and the touch / key event translation.
- `crates/gpui_android/examples/zed_android` — example application that
  composes Zed's real `Workspace` + `Project` + `MultiWorkspace` and
  bundles the result as an APK. The Kotlin host (`MainActivity`) is a
  ~100-line subclass of `GameActivity` that exposes SAF launchers to
  Rust over JNI.
- `crates/gpui_android/src/saf.rs` — translates between gpui's
  `prompt_for_paths` / `prompt_for_new_path` and Android's
  `ACTION_OPEN_DOCUMENT_TREE` / `ACTION_CREATE_DOCUMENT` intents. Tree
  URIs (`content://com.android.externalstorage.documents/tree/...`) are
  decoded back into POSIX paths under `/storage/emulated/0` so the rest
  of the editor (RealFs, worktrees, language server discovery, etc.)
  can use them unchanged.
- `crates/gpui_android/src/cursor.rs` — maps `gpui::CursorStyle` to
  Android's `PointerIcon` types via `View.setPointerIcon` over JNI, with
  a thread-local cache so we don't JNI-hop on every mouse move.

## Notes for hackers

A few things tripped us along the way that aren't obvious:

- `MANAGE_EXTERNAL_STORAGE` in the manifest plus `set_custom_data_dir`
  pointed at `app.internal_data_path()` is the simplest combo for
  letting RealFs roam over `/sdcard` while keeping Zed's own data
  (themes, settings, db) inside the app's private dir.
- gpui's `prompt_for_paths` is async via `oneshot::Receiver`, but
  Android's `ActivityResultLauncher` silently no-ops when called from a
  JNI thread that's not in the activity's STARTED state. We use the
  legacy `startActivityForResult` + `onActivityResult` path instead —
  same as AGDK's own SAF samples.
- The wgpu Vulkan surface is destroyed and recreated when the activity
  goes background → foreground (e.g. after the SAF picker returns).
  `unconfigure_surface` keeps the renderer + atlas alive across the
  hop; fully dropping the renderer leaves cached `AtlasTextureId`s
  dangling and the next paint indexes into an empty atlas.
- gpui's `cx.open_window` doesn't translate to Android (one window per
  task). The platform's `open_window` returns `Err` on the second call
  so callers see a graceful failure instead of `ERROR_NATIVE_WINDOW_IN_USE_KHR`.
  Welcome's "Open Project" handler is overridden to add the picked path
  as a worktree on the existing project rather than spawning a new
  workspace window.
- Long-press on touch is detected at the `MotionEvent::Up` boundary
  (≥500 ms hold, finger drift < 12 logical px). The buffered left-click
  is cancelled with `click_count: 0`, then a synthetic
  `MouseDown(Right)` + `MouseUp(Right)` fires so listeners that hook
  `on_secondary_mouse_down` (project panel context menu, tab close
  menu, etc.) get their callback.

## Storage workflow

Android partitions storage in a way that has direct consequences for a code
editor with an integrated terminal. Two facts shape everything below:

1. `/storage/emulated/0` (the user-visible "Internal storage") is
   FUSE-mounted with `noexec`. You can read, write, browse, and edit files
   there fine, but the kernel refuses to `execve()` anything that lives on
   it — `cargo run` against a binary in `/storage/emulated/0/projects/foo/
   target/debug/foo` returns `EACCES (Permission denied, os error 13)`.
2. `/data/data/com.zdroid/files` (app-private storage) is
   exec-mounted. At our pinned `targetSdk=28`, `execve()` and `dlopen()`
   work natively here — same place Termux runs everything from.

So projects live in app-private storage. Shared storage is for browsing,
single-file edits, and exporting back to the PC.

### Tier 1 — default

- `~/projects/<name>` is the workspace root. `cargo new`, `git clone`,
  `mkdir foo && cd foo && cargo init` all just work — exec is allowed,
  builds run, native modules dlopen cleanly.
- `~/storage/<name>` is a Termux-style curated symlink into shared storage:

  | symlink              | target                               |
  | -------------------- | ------------------------------------ |
  | `~/storage/shared`     | `/storage/emulated/0` (full sdcard) |
  | `~/storage/downloads`  | `/storage/emulated/0/Download`      |
  | `~/storage/dcim`       | `/storage/emulated/0/DCIM`          |
  | `~/storage/documents`  | `/storage/emulated/0/Documents`     |
  | `~/storage/movies`     | `/storage/emulated/0/Movies`        |
  | `~/storage/music`      | `/storage/emulated/0/Music`         |
  | `~/storage/pictures`   | `/storage/emulated/0/Pictures`      |
  | `~/storage/podcasts`   | `/storage/emulated/0/Podcasts`      |
  | `~/storage/external-N` | `/storage/<UUID>` (SD card / OTG)   |

  Use these for "open / edit / save a single file" workflows. Don't
  treat them as a workspace root — see Tier 1 above.
- **File → Import from sdcard…** runs the SAF folder picker, recursively
  copies the picked tree into `~/projects/<basename>`, and opens the
  imported copy. The original on shared storage stays untouched.
- If you do open a project rooted on shared storage anyway, the title
  bar shows a yellow **"Builds won't run · Move"** chip. Tap it to copy
  the project into `~/projects/<name>` and reopen there.

### Tier 2 — root (deferred)

Planned: a settings toggle that detects root, asks once, and `mount
--bind`s `/mnt/pass_through/0/emulated` (the underlying f2fs, exec-mounted)
over `~/sdcard-exec/`. Lets advanced users keep multi-GB projects on
shared storage and still build natively. Not implemented yet — waiting
on the in-app settings UI.

### Tier 3 — manual

`cd ~/storage/shared/projects/foo && mv ../foo ~/projects/`. Same
end state as the title-bar chip, no UI involved.

## Caveats

- Tree-shake is brutal: the .so is ~400 MB stripped for debug builds,
  ~100 MB for release. Most of that is wgpu, wasmtime (LSP / vim
  scripting), and Tree-sitter parsers. Release builds are usable; debug
  builds need the `[profile.dev] strip = "debuginfo"` override in
  `Cargo.toml` because `llvm-strip` chokes on the unstripped 2 GB+
  binary.
- LSP / DAP / collaboration are not enabled. The `livekit_client`,
  `audio`, and `call` crates have `target_os = "android"` cfg-gated to
  fall back to the freebsd / windows-gnu mock implementations, so they
  compile cleanly but no real functionality is wired.
- The soft keyboard isn't bridged yet. A hardware (BT) keyboard works.

## License

This experiment lives under the gpui_android crate and follows Zed's
existing GPL-3.0-or-later license. Zed itself is © Zed Industries.
