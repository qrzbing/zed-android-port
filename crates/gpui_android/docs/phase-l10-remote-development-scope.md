# Phase L10 — Remote development on Android: scope + plan

**Status:** Scoping (compile works, runtime untested with real SSH)
**TL;DR:** It's a light-touch port. The `remote` / `remote_connection` /
`remote_server` crates already compile for our Android target, the
`OpenRemote` action handler is registered via `recent_projects::init`
(which we already call), and the connection plumbing spawns a native
`ssh` subprocess rather than embedding a Rust SSH stack — so we just
need OpenSSH on the device's `$PATH` and a working remote-server
binary on the target host. No fork, libc, or PTY changes required in
the gpui_android platform layer.

## What production Zed does

1. **`zed_actions::OpenRemote`** action fires.
2. **`recent_projects::init`** registers a handler that opens
   `RemoteServerProjects` modal (SSH host + folder picker UI).
3. User picks/creates a connection — the modal calls
   `recent_projects::open_remote_project`, which routes through
   `workspace::open_remote_project_with_new_connection`.
4. **`remote::transport::ssh::MasterProcess`** spawns
   `util::command::new_command("ssh")` with OpenSSH's `ControlMaster`
   + `ControlPath` for connection multiplexing
   (`crates/remote/src/transport/ssh.rs:155-187`, non-Windows path).
5. **`remote_server`** binary is fetched on the remote — first via
   `wget`/`curl` from Zed's CDN on the remote host, fallback to
   client-side download + `scp` upload to `~/.cache/zed/`
   (`crates/remote/src/transport/ssh.rs:872-915`).
6. Wire format: length-prefixed protobuf `Envelope` messages over
   the SSH stdin/stdout tunnel (`crates/remote/src/protocol.rs:12-51`).

## What's already there on Android

- **All three remote crates compile for `aarch64-linux-android`**
  with no errors (verified via `cargo ndk` build at this scope).
- **`recent_projects::init(cx)` is called** at startup
  (`crates/gpui_android/examples/zed_android/src/lib.rs:474`), so
  the `OpenRemote` action handler is registered.
- **No platform-specific `cfg`s exclude Android** in any of
  `crates/remote/src/**`, `crates/remote_connection/src/**`. The
  only cfg gates we found are `target_os = "windows"` vs
  not-Windows; Android falls into the not-Windows path
  (`crates/remote/src/remote_client.rs:1365`,
  `crates/remote/src/remote.rs:8`,
  `crates/remote/src/transport.rs:346`).
- **`util::command::new_command("ssh")`** resolves via the
  process's `PATH`, which we prepend with `$PREFIX/bin`
  (Termux's userland) at boot
  (`crates/gpui_android/examples/zed_android/src/lib.rs:114-119`).
  When OpenSSH is installed in Termux, `ssh` resolves to
  `$PREFIX/bin/ssh`.
- **The `with_active_or_new_workspace` Android fallback we
  patched in L7g** routes the `RemoteServerProjects` modal back
  to the existing primary workspace instead of trying to spawn a
  duplicate, so the modal opens cleanly on tablet.

## What's missing / needs work

### L10a — OpenSSH not in default Termux bootstrap

`/data/data/dev.zed.zed_android/files/usr/bin/ssh` doesn't exist on a
fresh install. Termux ships OpenSSH as the `openssh` apt package, not
as part of the base bootstrap. Until that's installed, the SSH
subprocess spawn fails with `ENOENT`.

Three fixes, in order of effort:

1. **Document and let users install manually** (`apt install
   openssh` from Zed's terminal). 0 LOC. Bad UX for a flagship
   feature; user has to know to do this.
2. **Auto-install on first OpenRemote action invocation.** Detect
   `which("ssh")` at OpenRemote action time; if missing, run
   `apt install openssh` in the background, then proceed.
   Reasonable middle ground.
3. **Re-pack `termux-bootstrap.zip` to include openssh.** Modify
   the build pipeline that generates the bootstrap (currently
   sourced from upstream Termux) to install openssh during the
   pre-pack apt-cache stage. Cleanest UX, requires touching the
   bootstrap-build tooling.

Recommend (2) for ship — same install-on-first-use pattern Zed
already uses for tools like `npm` / `cargo`.

### L10b — Test connection flow on device

Compile works, runtime untested. The big unknowns:

- Whether the SSH `ControlPath` socket-path resolution works on
  Android. Zed picks a path from `tempfile::tempdir()`, which
  resolves via `TMPDIR` — we set that at boot, so likely fine.
- Whether `remote_server`'s auto-fetch on the remote host works
  with our typical setup (some remote hosts block outbound CDN
  fetches; the SCP-from-client fallback path then needs `scp` /
  `curl` on the Android client too — both would come with the
  openssh apt install).
- Whether the protobuf RPC stays alive across Android's
  background-app lifecycle. If the user backgrounds Zed, the SSH
  subprocess might get killed — we'd lose the connection. The
  `with_active_or_new_workspace` patch handles modal routing, but
  the underlying SSH process is a separate concern.

### L10c — `remote_server` binary cross-compile

The `remote_server` *target* runs on the remote, not the Android
client. So we don't need to cross-compile it for Android — we need
the Linux x86_64 / aarch64 / arm64 binaries that match the user's
remote host. Production Zed publishes those to its CDN and the
fetch logic at `crates/remote/src/transport/ssh.rs:872-915`
auto-discovers the right one. Our Android client doesn't change
this — same fetch / same binary.

### L10d — `crash-handler` / `fork` / `libc` on Android

`remote_server`'s `Cargo.toml` lists these under
`[target.'cfg(not(windows))'.dependencies]`, which includes
Android. They compile. They use Unix signals and `fork()`, both
of which Android's bionic supports. **But they're only used in
the `remote_server` binary that runs on the remote host**, not in
the client. Android client never invokes them. No work needed on
our side.

## Phase plan

**L10a** Auto-install OpenSSH on first OpenRemote action invocation
  (detect `which("ssh")`, prompt user, run `apt install openssh -y`
  in a background terminal). ~50 LOC in lib.rs / a new helper
  module. Defer until L10b proves out the rest.

**L10b** Build + install + test the connection flow end-to-end
  against a real remote SSH host. Watch for runtime panics, missing
  binaries, env var gaps. (DONE on the build side; needs the user
  on a device with `apt install openssh` already run.)

**L10c** Document common error modes and recovery in
  `crates/gpui_android/docs/workarounds/remote-development-on-android.md`.

**L10d** (optional) Re-pack `termux-bootstrap.zip` to include
  openssh by default, so first-launch ships ready-to-remote.

**L10e** (optional) Detect Android background lifecycle and
  surface a "remote disconnected" banner / auto-reconnect dialog
  if the SSH subprocess dies. Lower priority — most desktop SSH
  clients don't auto-reconnect either.

## Out of scope

- VS Code Server / Remote Tunnel via Microsoft's protocol — Zed has
  its own protocol and we'd need a translator. Not worth
  implementing; the user's framing was "Zed remote, like
  code-server but Zed-native".
- DAP / debugger over SSH — DAP is feature-flagged out on Android
  entirely, so debugger-over-remote inherits that.
- LiveKit / collab — those are real-time editing crates, also
  feature-flagged out on Android.
