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

### L10a — Bake openssh into the rebuilt bootstrap

`/data/data/dev.zed.zed_android/files/usr/bin/ssh` doesn't exist on a
fresh install. Termux ships OpenSSH as the `openssh` apt package, not
the base bootstrap, and `apt install openssh` from a running Zed
session pulls the upstream Termux deb which has
`/data/data/com.termux/files/...` paths baked into its binaries' rodata
(notably `libtermux-auth.so` which provides the `getpwnam` shim that
ssh-keygen consults for the default key path). Patchelf fixes the
ELF dynlinker metadata (RUNPATH, interp), but it can't rewrite
in-binary string literals — and the paths differ in length
(`/data/data/com.termux/files` = 27 chars vs `/data/data/dev.zed.zed_android/files`
= 36 chars), so an in-place hex-patch can't substitute either.

Three fixes considered:

1. ~~Auto-install at first OpenRemote invocation~~ — pulls the
   upstream-baked deb anyway. Same path-baked-in problem.
2. **Bake openssh into the bootstrap rebuild on the Vultr
   instance.** Same path L2a / `termux-bootstrap-rebuild.md`
   already takes for git, curl, and the rest of the prebundled
   userland: rebuild openssh's deb against our package-name
   substitution before packing into `termux-bootstrap.zip`. ssh,
   scp, sftp, ssh-keygen, sshd, and `libtermux-auth.so` all ship
   with `/data/data/dev.zed.zed_android/files/...` baked correctly
   from the start. No runtime patching, no Magisk root, no
   per-process mount namespace gymnastics.
3. ~~Symlink `/data/data/com.termux` → ours, or
   namespace-bind-mount~~ — requires Magisk root, conflicts with
   real Termux being installed alongside our app, and only fixes
   things at runtime per binary. Decisively worse than baking it
   in.

**Path forward: option (2).** Add `openssh` (+ its libtermux-auth
dep) to the bootstrap-rebuild package list on the Vultr
instance. Re-pack. Ship the bigger `termux-bootstrap.zip`. Same
mechanism that gave us git, npm, claude-code, etc. with paths
substituted at build time.

### L10a-tmp — Temporary user workaround (until next bootstrap ship)

If a tester needs to validate the rest of the L10 connection flow
before the rebuilt bootstrap lands:

```sh
apt install openssh
$PREFIX/etc/apt/zed-launcher-gen.sh   # patchelf RUNPATH
mkdir -p ~/.ssh
ssh-keygen -t ed25519 -N '' -f ~/.ssh/id_ed25519
```

The explicit `-f` arg sidesteps libtermux-auth's hardcoded default
(which still says `/data/data/com.termux/files/home/...`). Once
the Vultr-rebuilt bootstrap lands, the `-f` arg is no longer
necessary — defaults resolve to our home cleanly.

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

**L10a** Bake openssh into the bootstrap rebuild on the Vultr
  instance. Add `openssh` (+ `libtermux-auth.so` already pulled in
  transitively) to the package list, re-substitute paths,
  re-pack `termux-bootstrap.zip`. First-launch users get ssh /
  scp / sftp / ssh-keygen / sshd ready to use with our package
  paths baked correctly. Tracking inflight on the rebuild server.

**L10b** Build + install + test the connection flow end-to-end
  against a real remote SSH host. (Build side DONE; runtime
  needs a device with the rebuilt bootstrap, OR the L10a-tmp
  workaround applied for early validation.)

**L10c** Document common error modes and recovery in
  `crates/gpui_android/docs/workarounds/remote-development-on-android.md`.

**L10d** (optional) Detect Android background lifecycle and
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
