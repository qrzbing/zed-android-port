# Remote development on Android — env, channel, and CDN gotchas

**Status:** Active (working end-to-end against a Linux-x86_64 Vultr remote
from Tab S9 Ultra; Zed protocol, file tree, terminal, file ops all flow)
**Phase / Commit:** L10b
**Files:**
- `crates/gpui_android/examples/zed_android/src/lib.rs`
- `crates/gpui_android/examples/zed_android/Cargo.toml`
- `crates/remote/src/transport/ssh.rs`

## The one-line summary

Three things stand between an Android-built Zed and a working Open Remote
flow: the SSH transport's source-build fallback (which would need
rustup + zig + cargo-zigbuild on Termux), the auto-updater (uninitialized
on Android because we self-distribute via APK, but the remote_server
URL resolver depends on it), and Zed's worktree-shell env capture
(which forwards every Android-flavored env var across the SSH tunnel
and breaks the remote shell). All three are dealt with in lib.rs's
boot-time env setup + a small filter in `transport/ssh.rs::build_command_posix`.

## Symptom catalogue

### A. "rustup not found on $PATH, install rustup"

Path: `crates/remote/src/transport.rs:297-299`. Fires when
`build_remote_server_from_source` runs because Zed's CDN doesn't have a
prebuilt remote_server matching the client version. Requires
`rustup`/`zig`/`cargo-zigbuild` on the **client** to cross-compile a
remote_server for the target's triple. Fix: set
`ZED_BUILD_REMOTE_SERVER=never` so the source-build fallback returns
`Ok(None)` and Zed falls through to the CDN-or-existing-on-remote path.

### B. "ZED_BUILD_REMOTE_SERVER is not set and no remote server exists at …"

Path: `crates/remote/src/transport/ssh.rs:850-855`. After (A) is
disabled, the SSH transport hits the
`ReleaseChannel::Dev → bail!` branch — Dev channel hard-refuses CDN
download by design. Fix: set `ZED_RELEASE_CHANNEL=nightly` so the
match arm becomes `wanted_version = None` and Zed downloads the
latest-on-CDN binary regardless of the client version. Caveat: Zed's
data dir is namespaced by channel, so flipping it makes settings.json
/ recent_projects / ssh_connections look "fresh" on first launch (the
Dev-channel data is still on disk, just not loaded). Trade-off
acknowledged.

### C. "auto-update not initialized"

Path: `crates/auto_update/src/auto_update.rs:516`. Even with (A) + (B)
the CDN download path needs `GlobalAutoUpdate` initialized to resolve
the GitHub Releases URL for the remote_server asset. We don't normally
init `auto_update` on Android (distribution = user reinstalls APK).
Fix: call `auto_update::init(client.clone(), cx)` in `lib.rs` after
constructing `Client::production`, and pre-set
`ZED_UPDATE_EXPLANATION` so the polling subscription
(`auto_update.rs:248-262`) is suppressed — we don't want Zed
periodically self-updating the APK.

### D. `ld.so: object '/data/data/dev.zed.zed_android/files/usr/lib/libtermux-exec.so' from LD_PRELOAD cannot be preloaded: ignored.`

Sprayed on every command run via `remote_server` RPC. Caused by Zed's
worktree-shell env capture
(`crates/project/src/environment.rs::local_directory_environment`)
inheriting our entire Termux env from `profile.d/termux-exec.sh`, then
the SSH transport at
`crates/remote/src/transport/ssh.rs::build_command_posix`
forwarding it across the tunnel via
`exec env LD_PRELOAD=… HOME=… TERMUX__HOME=… ./remote_server`. Glibc
on the remote can't dlopen `/data/data/dev.zed.zed_android/...` (path
doesn't exist on Linux) and complains. **Bigger problem**: `HOME` and
`TMPDIR` were also being overridden, breaking remote `~/.ssh/`,
`~/.bashrc`, subprocess scratch space.

Fix: filter known-Android-only env vars in `build_command_posix`
before they reach the `exec env` invocation. Strip:
`LD_PRELOAD`, `HOME`, `TMPDIR`, `PREFIX`, `SSL_CERT_FILE`,
`CURL_CA_BUNDLE`, anything starting with `TERMUX__` or `TERMUX_APP__`.
Generic dev env (NVM_DIR, PYENV_ROOT, RUSTUP_HOME, etc.) keeps
forwarding so non-Android Zed clients aren't regressed.

### E. (Pre-existing) Open Remote args needed for libtermux-auth's broken HOME

Apt-installed openssh has `/data/data/com.termux/files/home` baked
into `libtermux-auth.so`'s `getpwuid` shim. Zed's ssh subprocess
respects this baked path over `$HOME`, so without override args
ssh-keygen / known_hosts / id_ed25519 all resolve to the wrong
location. Workaround in `settings.json`:

```json
{
  "ssh_connections": [{
    "host": "<your.host>",
    "username": "<user>",
    "args": [
      "-o", "UserKnownHostsFile=/data/data/dev.zed.zed_android/files/home/.ssh/known_hosts",
      "-o", "IdentityFile=/data/data/dev.zed.zed_android/files/home/.ssh/id_ed25519",
      "-o", "StrictHostKeyChecking=accept-new"
    ]
  }]
}
```

Permanent fix is L10a — bake openssh into the bootstrap rebuild on
the Vultr instance with our package paths substituted at build time.
After that the args block can come out of settings.json.

## What the local app sets up (boot-time, in `lib.rs`)

```rust
std::env::set_var("ZED_BUILD_REMOTE_SERVER", "never");      // (A)
std::env::set_var("ZED_RELEASE_CHANNEL", "nightly");        // (B)
std::env::set_var(                                          // (C, suppress polling)
    "ZED_UPDATE_EXPLANATION",
    "Updates ship via the APK; reinstall to upgrade.",
);
std::env::remove_var("LD_PRELOAD");                         // partial (D), kept for our process
auto_update::init(client.clone(), cx);                      // (C, init for URL resolver)
```

The `LD_PRELOAD` removal isn't strictly required after the (D) filter
landed in `transport/ssh.rs`, but kept for hygiene — it ensures any
non-SSH subprocess we spawn (cargo invocations, language servers run
locally, etc.) doesn't see a path that's about to break on the next
shell startup re-source.

## What the upstream Zed code change does

`crates/remote/src/transport/ssh.rs::build_command_posix` filters
input_env before assembling the `exec env K=V K=V ./remote_server`
invocation. The filter is keyed on env-var name — strips a fixed
list of Android/Termux-only names, plus any `TERMUX__` /
`TERMUX_APP__` prefix. Non-Android Zed clients are unaffected because
those env vars don't exist in their environment.

## Verifying the fix on device

After the build/install, in Zed's remote terminal (post-Open-Remote):

```sh
# SHOULD print empty brackets:
echo "LD_PRELOAD=[$LD_PRELOAD]"
# SHOULD print /root (or whatever the remote user's home is), NOT
# /data/data/dev.zed.zed_android/...:
echo "HOME=$HOME"
# SHOULD print /tmp or similar, NOT /data/data/.../usr/tmp:
echo "TMPDIR=${TMPDIR:-/tmp}"
# Smoke test: any command should run cleanly, no ld.so spam:
ls; pwd; whoami
```

If LD_PRELOAD shows up despite the filter, run:

```sh
ps -ef | grep zed-remote-server | grep -v grep
cat /proc/<remote_server_pid>/environ | tr '\0' '\n' | grep -E 'LD_PRELOAD|TERMUX'
```

That tells you whether it's leaking from the transport's `exec env`
line (filter regressed) or from the remote's own bash startup
(separate problem — check `~/.bashrc`, `/etc/profile.d/`).

## Failure modes if regressed

- **(D) filter list too narrow**: a new Termux env var leaks across.
  Add it to the filter at `transport/ssh.rs::build_command_posix`.
  Logcat won't show this; check the remote shell's `cat /proc/$$/environ`.
- **(D) filter list too wide**: a legit user env var (NVM, etc.) gets
  stripped. Symptom: that toolchain doesn't work on remote. Check the
  filter; only Android-only names should be in it.
- **(B) channel switch undone**: Open Remote starts hard-bailing again
  on Dev channel. settings.json from Dev-channel state will look
  empty; original is on disk under the Dev-channel data dir
  (different namespace).
- **(C) auto_update::init removed**: every Open Remote attempt bails
  with "auto-update not initialized". Re-add the call in lib.rs.
- **(A) ZED_BUILD_REMOTE_SERVER unset**: client tries to cross-compile,
  fails on missing rustup/zig. Re-set the var in lib.rs.

## See also

- [phase-l10-remote-development-scope.md](../phase-l10-remote-development-scope.md)
  — original scope doc; L10a (bootstrap rebuild for openssh) is the
  permanent fix for symptom (E)
- [termux-bootstrap-rebuild.md](termux-bootstrap-rebuild.md) — the
  bootstrap-rebuild process L10a feeds into
