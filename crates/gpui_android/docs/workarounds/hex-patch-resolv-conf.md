# Hex-patch `/etc/resolv.conf` → `/sdcard/.zed/r`

**Status:** Active
**Phase / Commit:** L4g (`ce613cb8fe`) + LD_PRELOAD strip (`5accf13755`) + libc patch (current commit)
**Files:** `crates/gpui_android/src/termux_bootstrap.rs` — `install_npm_launcher_generator` body (`patch_resolv_conf`, `handle_elf`) for binaries; `install_musl_linker` + `patch_resolv_conf_in_bytes` for the shipped musl libc.

## Problem

Bun-compiled CLIs (claude, codex, every static-musl tool) hardcode the
literal `/etc/resolv.conf` in their `.rodata`. They link musl libc
**statically**, so c-ares' `fopen("/etc/resolv.conf")` goes
direct-to-syscall — **no PLT, no GOT, nothing for LD_PRELOAD to
intercept**. Android has no `/etc/resolv.conf` (DNS is via netd/Java
APIs). Without intervention: no nameservers, no DNS, no API calls.

## Constraint

The historical workaround was proot bind: `proot -b
$PREFIX/etc/resolv.conf:/etc/resolv.conf`. Worked, but every syscall
the binary made got ptraced — including the ~thousands per second of
DNS retries from claude's broken Statsig telemetry client (upstream
issue #15384, closed "not planned"). End result: 5+ minute startup,
laggy keyboard, mouse disappearing. Unusable.

We can't write to `/etc/` on Android (read-only system partition). We
can't bind-mount without `CAP_SYS_ADMIN` (untrusted apps don't have
it). We can't override port 53 in c-ares (compile-time hardcoded). We
can't patch `process.platform`-style env tricks because c-ares parses
no env for the resolv path.

## Solution

**Patch the binary's `.rodata` literal directly.** `/etc/resolv.conf`
is 16 bytes including the trailing NUL. Replace those 17 bytes with
`/sdcard/.zed/r` + 3 NUL pad — **same byte width**, c-ares'
strlen-based length-determination naturally truncates at the first
NUL, so it opens `/sdcard/.zed/r` instead.

The launcher generator's perl block:

```perl
while ($data =~ /\x00\/etc\/resolv\.conf\x00/g) {
    my $offset = $-[0] + 1;        # offset of the leading '/'
    seek $fh, $offset, 0;
    print $fh "/sdcard/.zed/r\x00\x00";  # 14 chars + 2 NUL = 16 bytes
}
```

Idempotent: already-patched binaries match no occurrences, perl exits
cleanly. Same idempotency shape as our `NODE_PLATFORM` patch.

The `gpui_android::dns_bridge` module populates `/sdcard/.zed/r` at
boot from Android's `ConnectivityManager.getLinkProperties().dnsServers`
— see [jni-dns-bridge.md](jni-dns-bridge.md). So the file exists with
real, current nameservers when the patched binary opens it.

## The LD_PRELOAD strip companion

After hex-patching the resolv path, the binary still inherits the
process tree's `LD_PRELOAD=$PREFIX/lib/libtermux-exec.so` (set in
`lib.rs` for shebang-translation correctness on upstream Termux
packages). libtermux-exec.so is **bionic-linked** — calls
`__system_property_get`, `__register_atfork`, FORTIFY `_chk` symbols.
musl's linker can't resolve those, fails with:

```
Error relocating libtermux-exec.so: __register_atfork: symbol not found
... (etc)
```

Fix: `handle_elf` writes a tiny wrapper at `$PREFIX/bin/<basename>`
for any binary whose interpreter is our musl linker:

```sh
#!$PREFIX/bin/sh
exec env -u LD_PRELOAD "<patched-binary-path>" "$@"
```

Same `env -u LD_PRELOAD` the old proot wrapper had, just minus the
proot. Also bypasses the npm JS dispatcher (claude.exe → spawn) and
goes straight to the binary — single Node-less exec, faster cold
start.

## Two-layer patch — the binary AND the libc

Patching just the binary is not enough. Bun's HTTP/fetch reaches
`getaddrinfo()` through the **dynamically-loaded musl libc** we ship
at `$PREFIX/lib/libc.musl-aarch64.so.1`, and musl's
`__resolvconf` has its OWN baked-in `/etc/resolv.conf` literal —
unrelated to the static c-ares slot in the main binary.

Concrete bite (2026-05-06): claude's main binary's resolv literal
was patched correctly (`strings | grep -c /etc/resolv.conf` → 0),
but `claude --print "hi"` still hit `ECONNREFUSED`. Strace pinned it:

```
openat("/etc/resolv.conf", O_RDONLY) = -1 ENOENT
sendto(..., port=53, addr=127.0.0.1) = 35
... API Error: Unable to connect to API (ConnectionRefused)
```

Path was opened from inside musl libc, not the main binary. The fix
is to also hex-patch the libc asset bytes in `install_musl_linker`
before writing them out — same 16-byte-slot replacement, applied
once at boot. See [musl-linker-bundle.md](musl-linker-bundle.md) for
the libc-side write-up.

## What this kills vs the proot path

| | proot wrap | hex-patch + env-strip wrapper |
|---|---|---|
| Startup `--version` | 0.48s | 0.12s |
| Interactive cold start | 5+ minutes (telemetry storm) | ~10s (mostly Bun runtime init) |
| ptrace overhead | per-syscall | zero |
| Telemetry storm impact | catastrophic (every DNS query × ptrace) | normal (just outbound UDP) |
| Wrapper layers | proot wrap → JS → spawn → binary | env-strip → binary |
| Subprocess interception | proot follows fork/exec | none |

## Failure modes if regressed

- `find` filter loses `! -name '*.real'` again → wrapper-the-wrapper
  cycle returns. See `cleanup_legacy_claude_wrapper` for the unwind.
- `head -c 2` skip check broken → already-patched wrappers get
  re-patched (the cycle bug we fixed in commit `3f6c0eb77b`).
- `/sdcard/.zed/r` not populated by `dns_bridge` (boot init failed) →
  c-ares opens an empty file, gets no nameservers, falls back to
  127.0.0.1 → localhost has no DNS service → all queries fail. Tool
  errors with "ENOTFOUND" or similar.
- LD_PRELOAD strip removed → patched musl binary fails relocation on
  bionic-only symbols at first exec.

## See also

- [jni-dns-bridge.md](jni-dns-bridge.md) — the file-population side
- [sdcard-dot-zed-namespace.md](sdcard-dot-zed-namespace.md) — why
  `/sdcard/.zed/` and not app-private
- [npm-intercept.md](npm-intercept.md) — the launcher-gen
  infrastructure this rides on
- [claude-bun-binary-patchelf.md](claude-bun-binary-patchelf.md) — the
  superseded proot-wrap predecessor
