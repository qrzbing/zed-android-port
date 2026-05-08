# Zdroid Spawn Daemon — Magisk module

Persistent root-context spawn daemon for [Zdroid](../../../../../README.md)'s
chroot adapter. Required if you want Zdroid to route every spawn (LSPs,
git, formatters, terminal subprocesses) through a chroot rootfs without
paying Magisk `su` mediation per call.

## What it does

Without this module, Zdroid's chroot adapter calls `/product/bin/su -c
"..."` per spawn. Magisk's su daemon serializes those mediations, so a
burst of spawns (Zed startup fires hundreds) queues up and the device
fork-bombs.

This module starts a small C daemon (`zd-spawnd`, ~300 LOC) at boot in
Magisk's root context. The daemon listens on a Unix socket
(`/data/data/com.zdroid/files/run/zd-spawn`). Zdroid connects per spawn,
sends the request + stdio fds via `SCM_RIGHTS`, the daemon forks a
child, the child chroots into the rootfs, exec's the target. Per-spawn
cost: ~5ms instead of ~200ms.

Wire protocol: see `PROTOCOL.md` in the parent directory.

## Install

1. Download the latest `zdroid-spawnd-vX.Y.Z.zip` from
   [releases](https://github.com/Dylanmurzello/zed-android-port/releases).
2. In Magisk Manager: **Modules → Install from storage → pick the zip**.
3. Reboot.

After reboot, check the daemon is running:
```sh
adb shell su -c 'ps -ef | grep zd-spawnd'
```
Expect a `zd-spawnd <zdroid-uid>` line.

Logs live at `/data/adb/modules/zdroid_spawnd/log/zd-spawnd.log`. The
supervisor restarts the daemon if it crashes.

## Uninstall

In Magisk Manager: **Modules → Zdroid Spawn Daemon → Remove**, reboot.
Zdroid's chroot adapter falls back to the slow per-call su path.

## Troubleshooting

**Daemon doesn't start:** check logs at
`/data/adb/modules/zdroid_spawnd/log/zd-spawnd.log`. Common causes:
- Zdroid not installed yet at boot — service.sh waits 120s for
  `getprop sys.boot_completed = 1` then bails if `com.zdroid` isn't
  in `/data/data/`. Reboot once Zdroid is installed.
- Magisk version too old (need v20.4+).

**Spawn requests fail with "connect refused":** the daemon process died.
Check the log; the supervisor should auto-restart with a 5s backoff. If
restarts thrash, file a bug with the log attached.

**Permission denied on the socket:** the daemon sets the socket to
`mode 0660` owner `root:<zdroid-uid>`. If Zdroid was reinstalled with a
new uid (uncommon), reboot to let service.sh re-resolve and rebind.

## Build from source

The daemon is built from `../zd-spawnd.c`:

```sh
cd /path/to/zed-android-port/crates/gpui_android/native/zd-spawnd
make
```

Then `make magisk-module` packages this directory plus the binary into
`zdroid-spawnd-vX.Y.Z.zip`.
