# Android port — workarounds library

Index of every non-obvious thing we do to make Zed run on Android. Each entry
points at a deeper writeup explaining the problem, the constraint that rules
out the obvious fix, and the chosen approach. **This file is the table of
contents; the writeups live alongside it.**

When a new constraint hits us and the workaround is non-trivial, add an entry
here so future sessions don't re-derive it from scratch. When a workaround
is replaced by something cleaner, mark it Reverted and link to the
replacement.

## How to read this

| Status | Meaning |
|---|---|
| **Active** | Currently shipping in the APK |
| **Deferred** | Designed, scheduled, not yet built |
| **Reverted** | Was active, replaced by something cleaner |

## Storage / filesystem

| Workaround | Status | Why it exists |
|---|---|---|
| [Noexec mount on /storage/emulated/0](android-noexec-mount.md) | Active | FUSE-mounted shared storage rejects execve regardless of file mode |
| [`targetSdk=28` for execve from /data/data](targetsdk-28-execve.md) | Active | API 29+ blocks execve on app-private storage |
| [Termux ~/storage curated symlinks](termux-storage-symlinks.md) | Active | Surface common /sdcard subdirs without making them workspace roots |
| [~/projects workspace + Import-from-sdcard](projects-workspace-import.md) | Active | Builds need exec mount; SAF picks land on noexec mount |
| [Noexec banner with one-tap Move action](noexec-banner-move.md) | Active | When users open from /sdcard anyway, fix in one tap |
| [Trust grants restored from WorkspaceDb at boot](trust-restore-from-db.md) | Active | Production main.rs does this; we forgot |
| [Welcome-page Workspace/External split](welcome-page-split.md) | Active | Two `rust` projects from different storage tiers were indistinguishable |
| [Tier 2 root storage (bind-mount /mnt/pass_through)](deferred-tier2-root-storage.md) | Deferred | Wait for in-app settings UI |
| [`CARGO_TARGET_DIR` stopgap](reverted-cargo-target-dir.md) | Reverted | Per-tool env redirect didn't generalize |

## Termux integration

| Workaround | Status | Why it exists |
|---|---|---|
| [Termux bootstrap rebuilt with our package name](termux-bootstrap-rebuild.md) | Active | Upstream bootstrap hardcodes `/data/data/com.termux/...` in RUNPATH + shebangs |
| [dpkg path-rewrite patches](dpkg-path-rewrite-patches.md) | Active | `apt install` of upstream `.deb` files extracts paths under com.termux |
| [Apt Post-Invoke + Pre-Install hooks](apt-hooks.md) | Active | Layer maintainer-script rewrites + ELF RPATH fix on every dpkg op |
| [Apt dpkg pin](apt-dpkg-pin.md) | Active | Stop upstream apt from clobbering our patched dpkg |
| [Musl-aarch64 linker bundled in APK](musl-linker-bundle.md) | Active | Bun-compiled binaries reference `/lib/ld-musl-aarch64.so.1`; Android has no /lib |
| [Storage permission JNI shim](storage-permission-jni.md) | Active | `READ/WRITE_EXTERNAL_STORAGE` are runtime-prompted at targetSdk≤28 |

## Node / npm / CLI tools

| Workaround | Status | Why it exists |
|---|---|---|
| [Node binary `NODE_PLATFORM` patch](node-platform-patch.md) | Active | Termux Node is built `--dest-os=android`; npm picks wrong optional deps |
| [npm intercept stack (wrapper + launcher generator)](npm-intercept.md) | Active | Generic per-binary classification kills `zed-setup-X` per-tool sprawl |
| [Claude Bun-binary patchelf + proot wrapper](claude-bun-binary-patchelf.md) | Active | Bun static-musl with hardcoded `/etc/resolv.conf`, needs proot |
| [LD_PRELOAD `libzed-compat.so` path-redirect shim](deferred-ld-preload-shim.md) | Deferred | Replaces proot for *dynamic* binaries; needs build-time C compile |

## Runtime env

| Workaround | Status | Why it exists |
|---|---|---|
| [HOME env dual-pointing](home-env-dual-pointing.md) | Active | Rust process needs HOME=data_path; bash needs HOME=$TERMUX__HOME |
| [Terminal HOME override](terminal-home-override.md) | Active | Pass TERMUX__HOME into bash without disturbing Rust globals |
| [SSL_CERT_FILE / CURL_CA_BUNDLE](ssl-cert-bundle.md) | Active | Cargo / npm / curl don't know about Termux's CA bundle on Android |
| [.gitconfig safe.directory = *](gitconfig-safe-directory.md) | Active | libgit2 dubious-ownership check fires for media_rw-owned /sdcard repos |
| [Activity-recreation idempotency](activity-recreation-idempotency.md) | Active | `android_main` re-enters; everything must be re-entrant |
| [SELinux context canary log](selinux-canary.md) | Active | Detect if `targetSdk` regresses by checking `untrusted_app_27` domain |

## UI / input

| Workaround | Status | Why it exists |
|---|---|---|
| [Choreographer-driven vsync](choreographer-vsync.md) | Active | Replaces 8ms fixed-interval polling with event-driven vsync |
| [Two-finger tap → right click](two-finger-rightclick.md) | Active | Touchscreens don't have a right mouse button |
| [JVM stack overflow on clipboard](jvm-clipboard-stack-overflow.md) | Active | Android's 988KB android_main thread can't handle clipboard JNI synchronously |
| [Soft-keyboard / IME bridge](deferred-soft-keyboard.md) | Deferred | Hardware keyboard works; touch IME bridge is its own engineering problem |

## Build / packaging

| Workaround | Status | Why it exists |
|---|---|---|
| [Debug-strip oversized .so](debug-strip-oversized-so.md) | Active | llvm-strip chokes on >2 GB ELF; profile.dev workaround |
| [audio + livekit + call cfg-gates](android-cfg-gates.md) | Active | These crates don't compile against bionic; mock fallbacks already exist |
| [`platform.rs` no-drain RefCell pattern](refcell-drain-platform-bug.md) | Active | Draining main_receiver inside `open_window` panics on RefCell re-entry |

## Adding a new workaround

1. Hit a constraint that needs non-obvious work to solve.
2. Solve it.
3. Add a row to the right table above with a one-liner.
4. Create the linked `.md` next to this file using the template below.
5. Link it from the corresponding commit message.

## Template

```markdown
# <Title>

**Status:** Active | Deferred | Reverted
**Phase / Commit:** <which session this came from>
**Files:** <paths>

## Problem
<what was broken — the symptom>

## Constraint
<what rules out the obvious fix; primary-source links if relevant>

## Solution
<our approach, with the smallest possible code excerpt>

## Why this works
<the load-bearing invariant>

## Failure mode if regressed
<concrete observable symptom>

## See also
<related workaround entries>
```
