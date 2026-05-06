# Zed on Android

<p align="center">
  <em>The actual <a href="https://zed.dev">Zed</a> editor — workspace, project panel, multi-buffer editor, LSPs, terminal, git graph, extensions, remote SSH — running natively on an Android tablet.</em>
</p>

<p align="center">
  <!-- Replace USER/REPO once the GitHub repo is created. -->
  <img src="https://img.shields.io/badge/status-proof%20of%20concept-orange" alt="Proof of Concept" />
  <img src="https://img.shields.io/badge/platform-Android%208%2B-3DDC84?logo=android" alt="Android 8+" />
  <img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue" alt="License: GPL-3.0-or-later" />
  <img src="https://img.shields.io/github/v/release/USER/REPO?label=APK&sort=semver" alt="Latest APK" />
  <img src="https://img.shields.io/github/downloads/USER/REPO/total?label=downloads" alt="Total downloads" />
  <img src="https://img.shields.io/github/stars/USER/REPO?style=social" alt="GitHub stars" />
</p>

<p align="center">
  <!-- TODO: replace with the actual hero gif/video.
       45-60s loop showing: project open → search → terminal running cargo build →
       claude in terminal → git graph. Render to GIF for inline display, link to
       MP4 for full quality. -->
  <img src="docs/screenshots/hero.gif" alt="Zed on Android — workspace + terminal + LSP demo" width="100%" />
</p>

> **Proof of concept.** Not affiliated with Zed Industries. The editor works; rough edges are honest. See [Caveats](#caveats).

---

## What it is

Real Zed: gpui rendering with Vulkan via wgpu, the upstream `Editor`, `Workspace`, `Project`, `MultiWorkspace`, `Search`, `GitPanel`, `GitGraph`, `Extensions` and `Terminal` crates running unchanged. Not a webview. Not Termux + SSH to a server. Not proot or a chroot. The actual Rust `.so` runs as the app process; gpui composites every pixel directly via the Adreno Vulkan driver.

The trick: a custom `gpui_android` platform backend (Vulkan surface lifecycle, JNI bridges, touch/keyboard event translation) plus a Termux userland rebuilt under our app's package name so apt, bash, git, ssh, node, go, openjdk, rust-analyzer all run inside the app's data dir. Everything else is upstream Zed.

## What works

<p align="center">
  <!-- TODO: 4-up screenshot grid. ~600px wide each, low PNG compression. -->
  <table>
    <tr>
      <td><img src="docs/screenshots/workspace.png" alt="Workspace + project panel + editor" /></td>
      <td><img src="docs/screenshots/terminal.png" alt="Integrated terminal running cargo build" /></td>
    </tr>
    <tr>
      <td><img src="docs/screenshots/git_graph.png" alt="Git graph view with commit history" /></td>
      <td><img src="docs/screenshots/extensions.png" alt="Extensions browse/install pane" /></td>
    </tr>
  </table>
</p>

| | Status |
| --- | --- |
| Vulkan rendering through wgpu (Adreno verified) | ✅ |
| Workspace shell (tabs, dock, status bar, multi-pane) | ✅ |
| Editor: multi-buffer, vim mode, syntax highlighting | ✅ |
| Project panel + worktrees + trust prompts | ✅ |
| Command palette + fuzzy file finder | ✅ |
| Buffer search + project-wide search + replace | ✅ |
| Git panel + git graph + commit history + diff view | ✅ |
| Integrated terminal (real Termux: apt, bash, ssh, node, go, claude-code) | ✅ |
| LSPs running natively on-device (rust-analyzer baked in; gopls / ts / pyright / jdtls install in 1 command) | ✅ |
| Extensions: browse, install, manage (themes, language configs, grammars, LSPs) | ✅ |
| Remote SSH projects (server picker pill in title bar; full feature parity) | ✅ |
| Anthropic Claude Code CLI runs natively | ✅ |
| Themes + icon themes + auto light/dark from system | ✅ |
| Hardware keyboard, mouse, trackpad, two-finger right-click, long-press right-click | ✅ |
| Multi-window via Android freeform / DeX desktop mode | ✅ |
| Soft keyboard / touch IME bridge | ⏳ deferred |
| 120Hz on 120Hz panels (currently locked 60Hz) | ⏳ deferred |
| Collab, AI panels, livekit | ❌ stubbed (heavy deps; PoC skipped them) |

## Hardware tested

Verified daily on a **Samsung Galaxy Tab S9 Ultra** (Snapdragon 8 Gen 2 / Adreno 740, Android 14, One UI 6). Compiles for any aarch64 Android 8+ with Vulkan 1.1; Adreno is the only driver exercised — Mali / Xclipse will run but may want shader tweaks.

Best experience needs a hardware keyboard. Tablet + Bluetooth keyboard, foldable in tablet mode, or DeX/desktop-mode session with monitor + keyboard + mouse all work great. Phone-sized screens technically run but are explicitly de-prioritized — see [`docs/workarounds/deferred-phone-form-factor-polish.md`](docs/workarounds/deferred-phone-form-factor-polish.md).

## Install (precompiled APK)

```sh
# Download the latest release APK from the GitHub releases page
adb install -r zed-android-<version>.apk
adb shell am start -n com.zdroid/.MainActivity
```

Or sideload via your file manager — Android will prompt to allow installs from unknown sources. The first launch extracts a ~250 MB Termux userland into the app's private data dir; takes about 30 seconds. Subsequent launches are instant.

## Build from source

You'll need:

- Rust toolchain with `aarch64-linux-android` (`rustup target add aarch64-linux-android`)
- [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk) (`cargo install cargo-ndk`)
- Android NDK r27 (`sdkmanager "ndk;27.0.12077973"`)
- Gradle 8+, `adb` on `$PATH`
- A device with USB debugging on

```sh
cd crates/gpui_android/examples/zed_android

# 1. Build the .so. NDK platform 26+ — earlier APIs lack libaaudio.
ANDROID_NDK_HOME=/path/to/ndk/27.0.12077973 \
  cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build

# 2. Build + install + launch.
cd android
gradle assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.zdroid/.MainActivity

# 3. Tail logs.
adb logcat -d | grep -E "zed_android|RustPanic|FATAL"
```

First build is ~10 min. Incremental Rust rebuilds ~20 s, Gradle re-pack a few seconds.

## Architecture

The interesting work was at the Android boundary. Deep-dives on every workaround live under [`docs/workarounds/`](docs/workarounds/) — `README.md` there is an index. Some highlights:

- **Termux rebuilt under `com.zdroid`** — full apt/dpkg/bash userland with our package name baked into RUNPATHs and shebangs, so the entire Termux package ecosystem works in-process.
- **`/etc/resolv.conf` hex-patch** — Bun-compiled CLIs (claude-code, codex) statically link c-ares with `/etc/resolv.conf` baked in; we rewrite the literal in their `.rodata` AND in our musl libc to point at `/sdcard/.zed/r`, populated by a JNI bridge from `ConnectivityManager.getActiveDnsServers()`.
- **AChoreographer-driven vsync** via NDK FFI (no JNI hop per frame).
- **Storage Access Framework integration** for SAF picker → POSIX-path translation.
- **Multi-activity OS-chromed extra windows** — DeX freeform / desktop windowing show our extra windows with Android's own task chrome.
- **Stack of `apply_runtime_patches`** at every boot — npm wrapper with `npm_config_libc=musl` injection, launcher-gen patchelf for Bun-musl binaries, askpass helper for SSH password prompts, profile.d shim for terminal subprocess env, auto-reload of `/sdcard/.zed/r` on network changes.

## Caveats

This is **a proof of concept**, not a polished product. Honest list of rough edges:

- **Soft keyboard not bridged.** Need a hardware keyboard (Bluetooth or USB-C). Phone-only users without an accessory cannot type.
- **60Hz rendering on 120Hz panels.** Frame work is fast; we just haven't opted into 120Hz via `ANativeWindow_setFrameRate` yet. See [`docs/workarounds/deferred-render-pipeline-perf.md`](docs/workarounds/deferred-render-pipeline-perf.md).
- **Some extension-shipped LSPs are glibc-only and won't run.** JVM-based (`jdtls`, `kotlin-language-server`), Node-based (`typescript-language-server`), and Python-based (`pyright`) LSPs all work via Termux's bionic-built runtimes — install via `pkg install openjdk-17` etc. Native glibc-only binaries (some Rust- or Go-built LSPs that extensions ship as glibc binaries) won't load without proot/glibc-runner; out of scope by design (root- and proot-free is a hard constraint).
- **No collab / AI / livekit panels.** `livekit_client`, `audio`, `call`, `agent_ui`, `copilot`, `language_models` are cfg-gated to mock impls so they compile but don't wire. PoC skipped them; ~50 MB of heavy deps for features that need cloud account integration anyway.
- **Sandboxed storage.** Projects under `~/projects/` (app-private, exec-mounted) are the supported workflow. `/sdcard/` is browsable via SAF + curated symlinks at `~/storage/` but is FUSE-`noexec` — `cargo run` against a binary there returns EACCES. The app surfaces a "Builds won't run · Move" banner and offers one-tap copy.
- **Tested on Tab S9 Ultra only.** Should work on any Vulkan 1.1 + Adreno device; Mali / Xclipse not tried. File issues with logcat dumps if you have other hardware.

## License

GPL-3.0-or-later, same as upstream Zed. The bundled `bootstrap-aarch64.zip` contains Termux-rebuilt packages — each under its own license (mostly BSD/MIT/Apache; gnupg/bash/coreutils are GPL). The Alpine-derived `ld-musl-aarch64.so.1` we bundle is MIT.

This is © Dylan Murzello, distributed under GPL-3.0-or-later. Zed itself is © Zed Industries.

## Acknowledgments

- **Zed Industries** for building gpui to be platform-agnostic enough that an Android port is "weeks of plumbing" instead of "months of rewrites."
- **The Termux project** for a decade of figuring out how to ship a Linux userland on Android. Most of our `apt install` machinery is their patches with our package name swapped in.
- **Alpine** for the tiny musl loader we bundle so Bun-compiled musl binaries (claude-code, codex) execve cleanly on bionic.
- **The wgpu / blade-graphics maintainers** for a Vulkan abstraction that just works on Adreno.

## Contributing

Issues, screenshots, hardware reports, and PRs welcome. Read [`docs/workarounds/README.md`](docs/workarounds/README.md) before adding a new platform shim — there's a good chance the issue you're hitting has a documented workaround already, and the doc explains the constraint that ruled out the obvious fix.
