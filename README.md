# Zed on Android

<p align="center">
  <em><strong>Zdroid</strong>. The actual <a href="https://zed.dev">Zed</a> editor workspace, project panel, multi-buffer editor, LSPs, terminal, git graph, extensions, remote SSH running natively on an Android tablet.</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-experimental-orange" alt="Experimental" />
  <img src="https://img.shields.io/badge/platform-Android-3DDC84?logo=android" alt="Android" />
  <a href="https://github.com/Dylanmurzello/zed-android-port/releases/latest"><img src="https://img.shields.io/github/downloads/Dylanmurzello/zed-android-port/total?label=downloads" alt="Total downloads" /></a>
</p>

> **Experimental.** Not affiliated with Zed Industries. Might be highly unstable. See [Caveats](#caveats) for what doesn't work yet.
>
> Three runtime modes ship: **Bootstrap** (default, no root; ~250 MB Termux-derived userland with `com.zdroid` baked in), **Kali chroot** (needs Magisk + a Kali NetHunter rootfs; real glibc), or **External Termux** (proxy through your existing Termux app). Pick one in onboarding; switch anytime in Settings. See [Userland](#userland) for the tradeoffs.
>
> _Soft fork of [zed-industries/zed](https://github.com/zed-industries/zed)._

---

<p align="center">
  <!-- TODO: hero gif. 45-60s loop: project open, search, terminal cargo
       build, claude in terminal, git graph. -->
  <img src="crates/gpui_android/examples/zed_android/docs/screenshots/hero.gif" alt="Zed on Android: workspace, terminal, LSP demo" width="100%" />
</p>

Zed compiled from source for Android. gpui rendering with Vulkan via wgpu. The upstream `Editor`, `Workspace`, `Project`, `MultiWorkspace`, `Search`, `GitPanel`, `GitGraph`, `Extensions`, and `Terminal` crates running unchanged. Not a webview (no shade at electron). Bypasses termux + SSH to a server (since we have our own bootstrap). The actual Rust `.so` runs as the app process; gpui composites every pixel (yes, you read that right) directly via the Adreno Vulkan driver.

The trick was basically a custom `gpui_android` platform backend (Vulkan surface lifecycle, JNI bridges, touch and keyboard event translation) plus a Termux userland rebuilt under our package, `com.zdroid` (hence the unofficial name **Zdroid**, also the launcher label). apt, bash, git, ssh, node, go, openjdk, rust-analyzer all run inside the app's data dir. Everything else is upstream Zed.

---

## <img src="https://api.iconify.design/lucide:layers.svg?color=%23999999&height=22" valign="middle" /> &nbsp;What works

<p align="center">
  <table>
    <tr>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/ssh_workspace.jpg" alt="Remote SSH workspace" /></td>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/git_graph.jpg" alt="Git graph" /></td>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/settings.jpg" alt="Settings" /></td>
    </tr>
  </table>
</p>

- **Editor.** Vulkan rendering, multi-pane workspace, vim mode, syntax highlighting, project panel, fuzzy file finder, command palette, buffer + project search.
- **Git.** Git panel with staging, full git-graph commit history, diff view.
- **LSPs.** rust-analyzer baked in. gopls, ts-server, pyright, jdtls install in one `pkg`/`npm`/`go install`.
- **Extensions.** Browse, install, manage. Themes, language configs, grammars, slash commands.
- **Remote SSH.** Server-picker pill in the title bar, persisted server list, native askpass.
- **Input.** Hardware keyboard, mouse, trackpad. Two-finger and long-press right-click.
- **Multi-window.** Android freeform and DeX, each extra window is a real Activity with OS chrome.
- **Edge-to-edge** rendering with content under the display cutout.
- **App menu bar** with nested submenus (Settings, Keymap, Themes, Extensions).
- **Theme follow** for system light/dark.
- **`ZedDocumentsProvider`** exposes the project root as a SAF volume, so other Android apps can browse Zed's worktrees.

---

## <img src="https://api.iconify.design/lucide:map.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Roadmap

Documented in [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/). PRs welcome.

- Soft keyboard / touch IME bridge.
- 120Hz on 120Hz panels (currently 60Hz).
- Mailbox present mode, FrameMetrics, ALooper spurious-wake hunt, touch-event chain shortening.

Out of scope for this proof of concept: collab, AI panels, livekit voice. Cfg-gated to mocks so the binary still compiles. Cloud-account features that need backend integration anyway.

---

## <img src="https://api.iconify.design/lucide:tablet-smartphone.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Tested on

Samsung Galaxy Tab S9 Ultra (Snapdragon 8 Gen 2 / Adreno 740, Android 16, One UI 8) is the daily driver. Compiles for any aarch64 Android 9+ with Vulkan 1.1, but only Adreno is exercised. Mali / Xclipse will run but may want shader tweaks.

A hardware keyboard is the supported config. Tablet plus Bluetooth keyboard, foldable in tablet mode, or DeX/desktop-mode with monitor and peripherals all work. Phones technically run but are de-prioritized; see [`docs/workarounds/deferred-phone-form-factor-polish.md`](crates/gpui_android/docs/workarounds/deferred-phone-form-factor-polish.md).

---

## <img src="https://api.iconify.design/lucide:download.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Install

```sh
adb install -r zed-android-<version>.apk
adb shell am start -n com.zdroid/.MainActivity
```

Or sideload via your file manager. Android prompts for unknown-source installs. On first launch you'll be asked to pick a **runtime adapter** (see [Userland](#userland) below). Picking _Bootstrap_ downloads a ~250 MB Termux userland from [`Dylanmurzello/zdroid-bootstrap`](https://github.com/Dylanmurzello/zdroid-bootstrap) and extracts it into the app's private data dir; takes about 30 seconds on a fast connection. Subsequent launches are instant.

---

## <img src="https://api.iconify.design/lucide:hammer.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Build from source

You'll need:

- Rust toolchain with `aarch64-linux-android` (`rustup target add aarch64-linux-android`)
- [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk) (`cargo install cargo-ndk`)
- Android NDK r27 (`sdkmanager "ndk;27.0.12077973"`)
- Gradle 8+, `adb` on `$PATH`
- A device with USB debugging on

```sh
cd crates/gpui_android/examples/zed_android

ANDROID_NDK_HOME=/path/to/ndk/27.0.12077973 \
  cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build

cd android
gradle assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.zdroid/.MainActivity

adb logcat -d | grep -E "zed_android|RustPanic|FATAL"
```

First build is around 10 minutes. Incremental Rust rebuilds are 20 seconds, Gradle re-pack a few seconds.

---

## <img src="https://api.iconify.design/lucide:cog.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Architecture

Deep-dives in [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/). Highlights:

- **AChoreographer-driven vsync** via NDK FFI. No JNI hop per frame.
- **SAF integration** for picker → POSIX-path translation.
- **Multi-activity OS-chromed extra windows.** DeX freeform shows extra windows with Android's own task chrome.

---

<a id="userland"></a>
## <img src="https://api.iconify.design/lucide:server.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Userland

The editor is bionic-linked and runs as the Android app process. Every subprocess it spawns (`bash`, `apt`, language servers, formatters, terminal shells, git, ssh) is routed through whichever **runtime adapter** the user picked in onboarding. Three adapters ship; they version independently of the editor APK.

| Adapter | What it is | Where it comes from |
|---|---|---|
| **Bootstrap** _(default, no root)_ | A Termux userland rebuilt under `com.zdroid` — apt/dpkg/bash with our package name baked into RUNPATHs and shebangs. ~250 MB extracted into the app's private data dir. Pure bionic, no glibc; same trade-offs as any Termux install. Apt + `pkg install` work for everything Termux ships. | Auto-downloaded from [`Dylanmurzello/zdroid-bootstrap`](https://github.com/Dylanmurzello/zdroid-bootstrap) on first selection. |
| **Kali chroot** _(needs Magisk)_ | Real glibc Linux. Every spawn goes over a Unix socket to `zd-spawnd` (a small privileged daemon) which does `fork` + `chroot` + `setuid` + `execve` on the editor's behalf. ~5 ms per spawn vs ~200 ms for `su`-mediated. All the Termux gotchas (`/usr/bin/env`, `/tmp`, `dlopen libfoo.so`) disappear because you're inside a real distro. | Flash the Magisk module from [`Dylanmurzello/zdroid-spawnd`](https://github.com/Dylanmurzello/zdroid-spawnd) + drop a Kali NetHunter aarch64 rootfs at `/data/local/nhsystem/kali-arm64`. |
| **External Termux** _(if you already use Termux)_ | Talks to your existing Termux app via `com.termux.permission.RUN_COMMAND` intents. Lighter footprint; your existing userland stays untouched. JNI Intent bridge is in progress (see [#36](https://github.com/Dylanmurzello/zed-android-port/issues)). | Install Termux from F-Droid; grant `RUN_COMMAND` to Zdroid. |

Switching is one tap (Settings → Android Runtime). Selection persists in `$PREFIX/etc/zd-runtime.toml`.

### When to pick which

- **Just want it to work, no root**: Bootstrap. Apt, npm, go install, rust-analyzer all work. The user-facing rough edges are: precompiled Bun CLIs (claude-code, codex) need `/etc/resolv.conf` hex-patches we ship; some glibc-only extension binaries don't run.
- **Have Magisk, want a real Linux**: Kali chroot. Everything you'd expect on Debian/Kali works as-is, no shimming needed. The chroot is shared with whatever else uses that NetHunter rootfs.
- **Already on Termux**: External adapter once it lands. Your `~/`, your packages, your shell history; Zdroid just spawns subprocesses there.

### Bootstrap adapter notes (the ones with the funny path stuff)

- **Claude Code.** `npm install -g @anthropic-ai/claude-code`, then `claude`. If npm or any later `pkg install` complains about unmet deps, run `apt --fix-broken install` *afterwards* to settle them. Don't run fix-broken on a fresh bootstrap before you've installed anything: apt will treat the pre-baked packages (go, openssh, etc.) as "unowned" and remove them.
- **DNS via `/sdcard/.zed/r`.** Bun-compiled CLIs statically link c-ares with `/etc/resolv.conf` baked into rodata. The bootstrap ships with a musl loader + Bun-binary patcher that rewrites the literal to point at `/sdcard/.zed/r`. The file is populated by JNI from Android's `ConnectivityManager.getActiveDnsServers()` at every boot. Full writeup in [`zdroid-bootstrap/docs/hex-patch-resolv-conf.md`](https://github.com/Dylanmurzello/zdroid-bootstrap/blob/main/docs/hex-patch-resolv-conf.md).

---

## <img src="https://api.iconify.design/lucide:folder-tree.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Storage workflow

**Click "Open Project" from the welcome screen.** Android's storage picker opens at the device's shared storage. Two ways through:

- **Stay in the picker, open the side menu, navigate to Zdroid → projects.** Zdroid's `DocumentsProvider` exposes `~/` as a SAF root. Anything you open from under there is in exec-mounted storage and builds run normally.
- **Pick anywhere on `/sdcard/`** (an existing project on internal storage). It opens in **restricted mode** — Android's shared storage is FUSE-mounted `noexec`, so `cargo build`'s output binary can't `execve()`. Read / edit / save still work. Zdroid shows a yellow **Builds won't run · Move** chip at the top; one tap copies the folder into `~/projects/<name>` once and reopens it from the exec-mounted side.

The two storage realms underneath:

1. `/data/data/com.zdroid/files/` (app-private, exposed as `~/`) is **exec-mounted**. Same place Termux runs everything from. `~/projects/<name>` is the default workspace root; `cargo new`, `git clone`, builds, debugs, integrated terminal subprocesses all run.
2. `/storage/emulated/0/` (Android's shared storage, also surfaced at `~/storage/`) is **FUSE-mounted `noexec`**. Read / write / edit fine; the kernel refuses `execve()`. `cargo run` against a binary on `/sdcard/...` returns `EACCES`.

| Where | What for |
| --- | --- |
| `~/projects/<name>` | Default workspace root. Builds, debugs, terminal subprocesses all run. |
| `~/storage/{shared,downloads,...}` | Curated symlinks into shared storage. For "open / edit / save a single file" workflows. Don't workspace-root these. |
| File → Open (any `/sdcard/` path) | Restricted mode. Use the yellow Move chip to promote into `~/projects/`. |
| File → Import from sdcard… | SAF folder picker. Recursively copies the chosen folder into `~/projects/<basename>`. |

---

## <img src="https://api.iconify.design/lucide:terminal.svg?color=%23999999&height=22" valign="middle" /> &nbsp;LSP install recipes

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
# and add an `lsp.jdtls.binary.path` override in settings.json.
```

Themes, grammars, and language configs from the Extensions pane always work. Some extension-shipped binaries are glibc-only and won't run on Android (see [Caveats](#caveats)).

---

<a id="caveats"></a>
## <img src="https://api.iconify.design/lucide:triangle-alert.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Caveats

This is just a proof of concept. No promises, might be highly unstable. The list isn't comprehensive.

- Soft keyboard not bridged. Hardware keyboard required for text input.
- Input has rough edges. Hardware keyboard, mouse, trackpad, and touch all work for most flows, but some keystrokes / pointer events may not register or behave consistently. Touch scroll and drag are the most fragile; keyboard is the most reliable.
- 60Hz on 120Hz panels. Haven't opted into 120Hz via `ANativeWindow_setFrameRate` yet.
- Some extension-shipped LSPs are glibc-only and won't run. JVM/Node/Python LSPs work via Termux's bionic runtimes.
- No collab / AI / livekit panels. Cfg-gated to mocks.
- Sandboxed storage. `/sdcard/` is FUSE-noexec; build inside `~/projects/`.
- MIUI / HyperOS aggressive battery management kills backgrounded Zed. Settings → Apps → Zed → Battery → "No restrictions".
- Tested on Tab S9 Ultra only. Mali / Xclipse not tried.

---

## <img src="https://api.iconify.design/lucide:file-text.svg?color=%23999999&height=22" valign="middle" /> &nbsp;License

GPL-3.0-or-later, same as upstream Zed. The Bootstrap-adapter zip (distributed from [`Dylanmurzello/zdroid-bootstrap`](https://github.com/Dylanmurzello/zdroid-bootstrap), not bundled in the APK) contains Termux-rebuilt packages each under its own license (mostly BSD/MIT/Apache; gnupg/bash/coreutils are GPL). The Alpine-derived `ld-musl-aarch64.so.1` inside it is MIT. The `zd-spawnd` daemon ([`Dylanmurzello/zdroid-spawnd`](https://github.com/Dylanmurzello/zdroid-spawnd)) is GPL-3.0-or-later.

© Dylan Murzello, distributed under GPL-3.0-or-later. Zed itself is © Zed Industries.

---

## <img src="https://api.iconify.design/lucide:handshake.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Acknowledgments

- [Zed Industries](https://zed.dev/) for [`gpui`](https://github.com/zed-industries/zed/tree/main/crates/gpui) being platform-agnostic enough that an Android port is plumbing rather than a rewrite.
- The [`wgpu`](https://github.com/gfx-rs/wgpu) and [`blade-graphics`](https://github.com/kvark/blade) maintainers for a Vulkan abstraction that just works on Adreno.
- [The Termux project](https://termux.dev/) for [a decade of Linux-on-Android](https://github.com/termux/termux-app). Most of our `apt install` machinery is their patches with the package name swapped.
- [Alpine Linux](https://alpinelinux.org/) for [musl libc](https://musl.libc.org/), which lets Bun-compiled musl binaries (claude-code, codex) execve cleanly on bionic.

---

## <img src="https://api.iconify.design/lucide:git-pull-request.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Contributing

Issues, screenshots, hardware reports, and PRs welcome. Read [`crates/gpui_android/docs/workarounds/README.md`](crates/gpui_android/docs/workarounds/README.md) before adding a platform shim. Good chance the issue you're hitting has a documented workaround already, with the constraint that ruled out the obvious fix.

---

## <img src="https://api.iconify.design/lucide:circle-help.svg?color=%23999999&height=22" valign="middle" /> &nbsp;So why this ?

Zed Industries' position on a mobile/tablet port: **not planned**.

- [#12039 IOS/Android Port](https://github.com/zed-industries/zed/issues/12039), open since May 2024.
- [#34633 start of termux build](https://github.com/zed-industries/zed/issues/34633), closed as "not planned" in Jul 2025.
- [#43207 gpui: On Android](https://github.com/zed-industries/zed/issues/43207), open in the GPUI Roadmap as "Wide Scope" since Nov 2025.

This repo is what those threads were asking for, built independently. The Termux build attempt failed because the upstream `wasmtime`/`cranelift` deps don't compile inside Termux. We sidestep that by building the APK on a desktop with `cargo-ndk` and running our own custom Termux userland in process. No fork of upstream-Zed-with-android-cfg is needed; the Editor, Workspace, Project, Search, GitGraph, Terminal, Extensions crates run unchanged. The work is at the platform boundary, documented in [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/).
