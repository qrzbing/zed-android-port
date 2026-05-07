# Zed on Android

<p align="center">
  <em>The actual <a href="https://zed.dev">Zed</a> editor workspace, project panel, multi-buffer editor, LSPs, terminal, git graph, extensions, remote SSH running natively on an Android tablet.</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-proof%20of%20concept-orange" alt="Proof of Concept" />
  <img src="https://img.shields.io/badge/platform-Android%208%2B-3DDC84?logo=android" alt="Android 8+" />
  <img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue" alt="License: GPL-3.0-or-later" />
  <img src="https://img.shields.io/github/v/release/Dylanmurzello/zed-android-port?label=APK&sort=semver" alt="Latest APK" />
  <img src="https://img.shields.io/github/downloads/Dylanmurzello/zed-android-port/total?label=downloads" alt="Total downloads" />
  <img src="https://img.shields.io/github/stars/Dylanmurzello/zed-android-port?style=social" alt="GitHub stars" />
</p>

<p align="center">
  <!-- TODO: hero gif. 45-60s loop: project open, search, terminal cargo
       build, claude in terminal, git graph. GIF for inline, MP4 link
       for full quality. -->
  <img src="crates/gpui_android/examples/zed_android/docs/screenshots/hero.gif" alt="Zed on Android: workspace, terminal, LSP demo" width="100%" />
</p>

> **Proof of concept.** Not affiliated with Zed Industries. The editor works; rough edges are honest. See [Caveats](#caveats).
>
> _This repo is a soft fork of [zed-industries/zed](https://github.com/zed-industries/zed); the upstream's own README is preserved at [`README.zed-upstream.md`](README.zed-upstream.md)._

---

## So why this ?

Zed Industries' official position on a mobile/tablet port: **not planned**.

- [#12039 IOS/Android Port](https://github.com/zed-industries/zed/issues/12039), open feature request since May 2024. iPad programming, tablet coding, mobile note taking. Triaged, not actioned.
- [#34633 start of termux build](https://github.com/zed-industries/zed/issues/34633), community attempt to compile Zed inside Termux. SIGSEGV in `cranelift-codegen`. **Closed as "not planned"** (Jul 2025).
- [#43207 gpui: On Android](https://github.com/zed-industries/zed/issues/43207), sits in the GPUI Roadmap as "Wide Scope" since Nov 2025.

This repo is what those threads were asking for, built independently. The Termux build attempt failed because the upstream `wasmtime`/`cranelift` dependencies don't compile inside Termux. We sidestep that by building the APK on a desktop with `cargo-ndk` and running our own custom Termux userland in process. No fork of upstream-Zed-with-android-cfg is needed; the Editor / Workspace / Project / Search / GitGraph / Terminal / Extensions crates run unchanged. The work is at the platform boundary, see [Architecture](#architecture).

## What it is

Zed compiled from the source for Android : gpui rendering with Vulkan via wgpu, the upstream `Editor`, `Workspace`, `Project`, `MultiWorkspace`, `Search`, `GitPanel`, `GitGraph`, `Extensions` and `Terminal` crates running unchanged. Its not a webview (no shade at electron). Bypasses termux + SSH to a server (since we have our own bootstrap). The actual Rust `.so` runs as the app process; gpui composites every pixel (yes, you read that right) directly via the Adreno Vulkan driver.

The trick was basically a custom `gpui_android` platform backend (Vulkan surface lifecycle, JNI bridges, touch/keyboard event translation) plus a Termux userland rebuilt under our app's package name so apt, bash, git, ssh, node, go, openjdk, rust-analyzer all run inside the app's data dir. Everything else is upstream Zed.

## What works

<p align="center">
  <!-- TODO: 4-up screenshot grid. ~600px wide each, low PNG compression. -->
  <table>
    <tr>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/workspace.png" alt="Workspace, project panel, editor" /></td>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/terminal.png" alt="Integrated terminal running cargo build" /></td>
    </tr>
    <tr>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/git_graph.png" alt="Git graph view with commit history" /></td>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/extensions.png" alt="Extensions browse and install pane" /></td>
    </tr>
  </table>
</p>

**Editor and workspace.** Vulkan rendering through wgpu (verified on Adreno). Multi-pane workspace with tabs, dock, status bar. Multi-buffer editor with vim mode and syntax highlighting. Project panel, worktrees, trust prompts. Fuzzy file finder. Command palette. Buffer search. Project-wide search with replace.

**Git tooling.** Git panel with staging. Full git-graph commit history viewer. Side-by-side diff view. Commit-history pane.

**Termux, in-process.** A complete Termux userland rebuilt under the app's package name, running inside the app process. apt, bash, ssh, node, go, git, python, openjdk all execute natively. No SSH bridge to a server. The integrated terminal opens straight into this environment.

**Language servers.** rust-analyzer is baked into the bootstrap. gopls, typescript-language-server, pyright, jdtls, and others install in one `pkg`/`npm`/`go install` from the integrated terminal (recipes below). Extension-shipped LSPs work as long as the underlying binary isn't glibc-only.

**Extensions.** Browse, install, and manage. Themes, icon themes, language configs, grammars, slash commands.

**Remote SSH.** Server-picker pill in the title bar, full SSH connection lifecycle, persisted server list. Native askpass helper handles password and passphrase prompts.

**Claude Code.** Anthropic's Claude Code CLI runs natively. `pkg install npm && npm install -g @anthropic-ai/claude-code`, then `claude` from the integrated terminal works.

**Input and windowing.** Hardware keyboard, mouse, trackpad. Two-finger and long-press right-click. App menu bar with nested submenus (Settings, Keymap, Themes, Extensions). Multi-window via Android freeform and DeX desktop mode (each extra window is a real Activity with OS chrome). Edge-to-edge rendering with content under the display cutout. System light/dark theme follow.

**Bonus.** ZedDocumentsProvider exposes the project root as a Storage Access Framework volume, so other Android apps can browse Zed's worktrees through their own file pickers.

## Roadmap

A few items are deferred. Each is documented in [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/) with the investigation path. PRs welcome.

- Soft keyboard / touch IME bridge. A hardware keyboard is required today.
- 120Hz on 120Hz-capable panels. Currently 60Hz; opt-in is via `ANativeWindow_setFrameRate`.
- Other render-pipeline polish: Mailbox present mode, FrameMetrics instrumentation, ALooper spurious-wake hunt, touch-event chain shortening.

Out of scope for this proof of concept: collab, AI panels, livekit voice. Cfg-gated to mock implementations so the binary still compiles. Cloud-account features that need backend integration anyway, deferred to post-PoC.

## Tested on

Samsung Galaxy Tab S9 Ultra (Snapdragon 8 Gen 2 / Adreno 740, Android 16, One UI 8) is the daily driver. Compiles for any aarch64 Android 8+ with Vulkan 1.1, but only Adreno is exercised. Mali / Xclipse will run but may want shader tweaks.

A hardware keyboard is the supported config. Tablet plus Bluetooth keyboard, foldable in tablet mode, or DeX/desktop-mode with monitor and peripherals all work. Phones technically run but are de-prioritized; see [`crates/gpui_android/docs/workarounds/deferred-phone-form-factor-polish.md`](crates/gpui_android/docs/workarounds/deferred-phone-form-factor-polish.md).

## Install (precompiled APK)

```sh
adb install -r zed-android-<version>.apk
adb shell am start -n com.zdroid/.MainActivity
```

Or sideload via your file manager. Android prompts to allow installs from unknown sources. The first launch extracts a 250 MB Termux userland into the app's private data dir; takes about 30 seconds. Subsequent launches are instant.

## Storage workflow

Android is strict with app storage. Two facts to know:

1. `/storage/emulated/0/` (the user-visible "Internal storage", linked at `~/storage/`) is FUSE-mounted noexec. You can read, write, browse, edit fine. The kernel refuses to `execve()` anything that lives there. `cargo run` against a binary in `/storage/emulated/0/projects/foo/target/debug/foo` returns `EACCES`.
2. `/data/data/com.zdroid/files/` (app-private storage, exposed as `~/`) is exec-mounted. `execve()` and `dlopen()` work natively, same place Termux runs everything from.

So:

| Where | What for |
| --- | --- |
| `~/projects/<name>` | Default workspace root. `cargo new`, `git clone`, `mkdir foo && cargo init` all just work. Exec is allowed, builds run, native modules dlopen cleanly. |
| `~/storage/<shared,downloads,dcim,...>` | Curated symlinks into shared storage. Use these for "open / edit / save a single file" workflows. Don't treat them as a workspace root. |
| File → Import from sdcard… | Runs the SAF folder picker, recursively copies the picked tree into `~/projects/<basename>`, opens the imported copy. Original on shared storage stays untouched. |
| Yellow "Builds won't run · Move" chip | Appears in the title bar if you open a project rooted on `/sdcard/` anyway. One tap copies the project into `~/projects/<name>` and reopens there. |

## LSP install recipes

Most LSPs install in a single command from the integrated terminal:

```sh
# Rust: baked into the bootstrap.
rust-analyzer --version

# Go.
go install golang.org/x/tools/gopls@latest

# TypeScript / JavaScript.
npm install -g typescript typescript-language-server

# Python (Pyright).
pkg install python && npm install -g pyright

# Java (jdtls). JVM-based, no native proxy needed.
pkg install openjdk-17 maven
# Then download jdtls from https://download.eclipse.org/jdtls/milestones/
# and add an `lsp.jdtls.binary.path` override in settings.json pointing
# at Termux's `java` and the launcher .jar.
```

Extensions can also install LSPs through the Extensions pane (settings menu → Extensions). Themes, grammars, and language configs always work from extensions. Some extension-shipped binaries are glibc-only and won't run on Android (see [Caveats](#caveats)).

## Build from source

You'll need:

- Rust toolchain with `aarch64-linux-android` (`rustup target add aarch64-linux-android`)
- [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk) (`cargo install cargo-ndk`)
- Android NDK r27 (`sdkmanager "ndk;27.0.12077973"`)
- Gradle 8+, `adb` on `$PATH`
- A device with USB debugging on

```sh
cd crates/gpui_android/examples/zed_android

# 1. Build the .so. NDK platform 26+ is required (libaaudio).
ANDROID_NDK_HOME=/path/to/ndk/27.0.12077973 \
  cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build

# 2. Build, install, launch.
cd android
gradle assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.zdroid/.MainActivity

# 3. Logs.
adb logcat -d | grep -E "zed_android|RustPanic|FATAL"
```

First build is around 10 minutes. Incremental Rust rebuilds are 20 seconds, Gradle re-pack a few seconds.

## Architecture

The interesting work was at the Android boundary. Deep-dives on every workaround live under [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/); `README.md` there is the index. Some highlights:

- Termux rebuilt under `com.zdroid`. Full apt/dpkg/bash userland with our package name baked into RUNPATHs and shebangs, so the entire Termux package ecosystem works in-process.
- `/etc/resolv.conf` hex-patch. Bun-compiled CLIs (claude-code, codex) statically link c-ares with `/etc/resolv.conf` baked into rodata. We rewrite the literal in the binary's `.rodata` and in our musl libc to point at `/sdcard/.zed/r`, populated by a JNI bridge from `ConnectivityManager.getActiveDnsServers()`.
- AChoreographer-driven vsync via NDK FFI. No JNI hop per frame.
- Storage Access Framework integration for SAF picker to POSIX-path translation.
- Multi-activity OS-chromed extra windows. DeX freeform / desktop windowing show our extra windows with Android's own task chrome.
- A stack of `apply_runtime_patches` at every boot. npm wrapper with `npm_config_libc=musl` injection, launcher-gen patchelf for Bun-musl binaries, askpass helper for SSH password prompts, profile.d shim for terminal subprocess env, auto-reload of `/sdcard/.zed/r` on network changes.

## Caveats

This is just a proof of concept. No promises, might be highly unstable. The list below isn't comprehensive; plenty still needs work.

- Soft keyboard not bridged. Hardware keyboard required.
- 60Hz on 120Hz panels. Frame work is fast; we just haven't opted into 120Hz via `ANativeWindow_setFrameRate` yet. See [`crates/gpui_android/docs/workarounds/deferred-render-pipeline-perf.md`](crates/gpui_android/docs/workarounds/deferred-render-pipeline-perf.md).
- Some extension-shipped LSPs are glibc-only and won't run. JVM-based (jdtls, kotlin-language-server), Node-based (typescript-language-server), and Python-based (pyright) LSPs all work via Termux's bionic runtimes; install them with `pkg install` etc. Native glibc-only binaries (some Rust- or Go-built LSPs that extensions ship as glibc binaries) won't load without proot or glibc-runner. Out of scope by design (root-free and proot-free is a hard constraint).
- No collab, AI, or livekit panels. `livekit_client`, `audio`, `call`, `agent_ui`, `copilot`, `language_models` are cfg-gated to mock impls. ~50 MB of heavy deps for cloud-account features anyway.
- Sandboxed storage. Projects under `~/projects/` (app-private, exec-mounted) are the supported workflow. `/sdcard/` is browsable via SAF and the curated symlinks at `~/storage/` but is FUSE-noexec. `cargo run` against a binary there returns EACCES. The app surfaces a "Builds won't run · Move" banner with one-tap copy. See [Storage workflow](#storage-workflow).
- MIUI / HyperOS aggressive battery management. Xiaomi/Redmi/Poco devices kill backgrounded Zed within minutes via `MIUI Optimization` / `Battery saver`. Workaround: Settings → Apps → Zed → Battery → "No restrictions". See [`crates/gpui_android/docs/workarounds/miui-aggressive-task-killing.md`](crates/gpui_android/docs/workarounds/miui-aggressive-task-killing.md).
- Tested on Tab S9 Ultra only. Should work on any Vulkan 1.1 + Adreno device. Mali / Xclipse not tried. Open issues with logcat dumps if you have other hardware.

## License

GPL-3.0-or-later, same as upstream Zed. The bundled `bootstrap-aarch64.zip` contains Termux-rebuilt packages, each under its own license (mostly BSD/MIT/Apache; gnupg/bash/coreutils are GPL). The Alpine-derived `ld-musl-aarch64.so.1` we bundle is MIT.

This is © Dylan Murzello, distributed under GPL-3.0-or-later. Zed itself is © Zed Industries.

## Acknowledgments

- [Zed Industries](https://zed.dev/) for building [`gpui`](https://github.com/zed-industries/zed/tree/main/crates/gpui) to be platform-agnostic enough that an Android port is plumbing rather than a rewrite.
- [The Termux project](https://termux.dev/) for [a decade of figuring out how to ship a Linux userland on Android](https://github.com/termux/termux-app). Most of our `apt install` machinery is their patches with the package name swapped in.
- [Alpine Linux](https://alpinelinux.org/) for the [musl libc](https://musl.libc.org/) we bundle so Bun-compiled musl binaries (claude-code, codex) execve cleanly on bionic.
- The [`wgpu`](https://github.com/gfx-rs/wgpu) and [`blade-graphics`](https://github.com/kvark/blade) maintainers for a Vulkan abstraction that just works on Adreno.

## Contributing

Issues, screenshots, hardware reports, and PRs welcome. Read [`crates/gpui_android/docs/workarounds/README.md`](crates/gpui_android/docs/workarounds/README.md) before adding a new platform shim. Good chance the issue you're hitting has a documented workaround already, and the doc explains the constraint that ruled out the obvious fix.
