# zd-spawnd wire protocol

Persistent root-context spawn daemon for Zdroid's chroot adapter. One
connection per spawn request; the connection lifetime tracks the
spawned process's lifetime.

## Why this exists

Magisk's `su` is **on-demand by design** (magiskd serializes mediation
per call to save idle CPU). Cost: ~200ms / spawn. Zed's startup fires
hundreds of spawns; the queue bunches up and the device fork-bombs.

zd-spawnd is started ONCE at boot via a Magisk module's `service.sh`,
inheriting Magisk's root context. After that, every Zdroid spawn is a
socket roundtrip + fork + chroot + exec — no per-call su mediation.

Cost per spawn: ~5ms instead of ~200ms.

## Socket

Path: `/data/data/com.zdroid/files/run/zd-spawn`

Filesystem socket (not abstract namespace) so SELinux labels and
filesystem permissions both gate access. Owner: `root:u0_a<zdroid_uid>`,
mode `0660`. Anyone outside Zdroid's UID can't connect.

The daemon also `getsockopt(SO_PEERCRED)` checks that the connecting
PID's UID matches Zdroid's UID before serving any request — defense
in depth against a misconfigured installer that opens the socket
permissions wider than intended.

## Frame format

All multi-byte integers are little-endian (matches host arm64). Strings
are UTF-8, NOT null-terminated; use the explicit length prefix.

### Request frame

```
struct request_header {
    uint32_t magic;       // 0x5A445350 ("ZDSP" little-endian)
    uint32_t version;     // protocol version, currently 1
    uint32_t flags;       // bit 0: interactive (allocate ctty)
                          // bits 1..31: reserved, must be 0
    uint32_t prog_len;    // bytes in target binary path
    uint32_t cwd_len;     // bytes in working directory path
    uint32_t argc;        // number of argv entries (1..N inclusive)
    uint32_t envc;        // number of envp entries
};

// Followed by:
//   prog_len bytes      — target binary, e.g. "/usr/bin/git" (absolute)
//   cwd_len bytes       — working directory, MUST exist inside chroot
//   argc × { uint32_t len; len bytes; }   — argv[1..]
//   envc × { uint32_t len; len bytes; }   — env vars in "KEY=VALUE" form
```

The first message must also carry exactly **3 file descriptors** in
the ancillary data via `SCM_RIGHTS`: stdin, stdout, stderr in that
order. The daemon dup2()s these in the child before exec.

### Response frame

The daemon sends one response per request. Two shapes:

```
struct response_spawned {
    uint32_t magic;       // 0x5A445350
    uint32_t version;     // matches request version
    int32_t status;       // 0 = spawn ok, child PID will be reported.
                          // negative = -errno (no child spawned).
    uint32_t child_pid;   // zd-spawnd's internal id for the child;
                          // not the host kernel pid (don't expose
                          // chroot kernel pid to clients)
};
```

If `status < 0`, no further messages; connection closes. The client
treats the negative errno as the spawn's exit code.

If `status == 0`, the daemon also sends the child's exit status when
the child eventually exits:

```
struct response_exited {
    uint32_t magic;
    uint32_t version;
    int32_t  exit_code;   // 0..255 normal exit
                          // negative = -signal (e.g. -9 if SIGKILL'd)
};
```

After `response_exited` the daemon closes the connection.

## Cancellation

Client wants to kill its in-flight spawn: shutdown the connection's
write half (`shutdown(fd, SHUT_WR)`). The daemon's read returns 0,
which it treats as "client asked us to terminate this spawn". Daemon
SIGKILLs the child, reaps it, sends `response_exited` with `-9`,
closes the connection.

## Daemon → systemd-style logging

zd-spawnd writes to `/data/adb/modules/zdroid-spawnd/zd-spawnd.log`
(persists across boots; rotated by Magisk module updates).

Log levels:
- `INFO` — boot, socket open, accept loop start.
- `WARN` — request decode error, peer cred mismatch, child exec ENOENT.
- `ERROR` — socket bind fail, fork fail, unrecoverable state.

## Client recovery

`zd-exec` (the chroot adapter) treats socket connection failure as a
fallback signal: if it can't reach the daemon, it falls back to the
old `RUNTIME_SU` per-call dispatch (slow but works). This lets the
adapter survive: first install before Magisk module activation, daemon
restart during a Magisk module update, kernel killing the daemon under
extreme memory pressure.

The fallback path logs a warning each time it fires so the user can
notice if the daemon's not coming up reliably.

## Versioning

Wire-incompatible changes bump the `version` field in the header and
the daemon refuses any request with a different `version`. Backward
compatibility is maintained by bumping a sub-minor without breaking
existing fields; minor bumps are reserved for additive optional
trailing fields (gated by a feature flag in `flags`).
