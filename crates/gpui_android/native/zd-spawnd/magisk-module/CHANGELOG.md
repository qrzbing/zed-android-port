# Zdroid Spawn Daemon changelog

## v1.1.4 (2026-05-10)

Reactive boot design + Magisk Remove revert.

### Fixes

* **service.sh now waits on the right signals, not blind retries.** v1.1.3 hit a 60s wall before the user unlocked the device for the first time post-boot. On Android FBE (file-based encryption), `/data/data/com.zdroid` is in user-0's credential-encrypted storage and unreadable until the user unlocks; `sys.boot_completed=1` fires before that. v1.1.4 watches three canonical AOSP signals instead:
  1. `sys.boot_completed=1` (system_server alive)
  2. `pm list packages -U com.zdroid` returning a uid (PackageManager-based, lives in DE space, no FBE wait needed for the lookup)
  3. `sys.user.0.ce_available=true` (set by `vold` in `OnUserUnlocked`, AOSP-universal across OEMs)
  
  Each `getprop` call is microseconds against Android's shared-memory property service, so the polls are cheap. When all three are met, the daemon starts. No timeouts on the operation, no "give up after N seconds". If Zdroid is reinstalled while the supervisor is waiting, it picks up the new uid automatically.

### New

* **`uninstall.sh`** — runs at the next boot after Magisk Remove (or `magisk --remove-modules`). Restores `/root/.bash_profile` and `/root/.profile` from the `.zdroid-orig` backups inside the chroot, and `rmdir`s the empty `/zed` bind-mount target. Magisk Remove now actually reverts; previously the chroot's bash startup stayed Zdroid-flavored forever. Audit log at `/data/adb/zdroid-spawnd-uninstall.log` survives the module dir removal.

## v1.1.3 (2026-05-10)

Three production fixes from real-device testing of v1.1.2.

### Fixes

* **Magisk Manager "Action" button now appears.** Magisk's button-render condition is presence of `action.sh` at the module root, not `webroot/`. v1.1.2 only had `webroot/` so the button never showed. Added `action.sh` that prints a one-shot status snapshot (daemon health, socket, bind mount, chroot patches, last log lines). Stdout streams to Magisk's in-app console.
* **WebUI no longer hammers Magisk's su grant ledger.** v1.1.2 polled status every 5s with 4 separate `ksu.exec` calls = 48 su grants per minute, spamming the Magisk log and the user's grant prompt history. v1.1.3 batches all status checks into a single shell command (one su grant per refresh) and removes background polling entirely. Refresh is manual, plus an automatic refresh after each action button.
* **service.sh boot race fixed.** `sys.boot_completed=1` fires before `/data/data/com.zdroid` is reachable on first boot after Zdroid install or major upgrade, so a single `stat -c %u` returned empty and the daemon never started. Replaced with a 60s retry loop that polls every second until the uid resolves. Most boots clear within 1-3s.

### Internal

* `customize.sh` now sets perms on `action.sh` alongside `service.sh` and `chroot-init.sh`.

## v1.1.2 (2026-05-10)

WebUI for module status, logs, and actions. Visible in KSU WebUI / MMRL. Magisk-only users without a WebUI viewer see no change.

### Highlights

* `webroot/index.html` panel exposes daemon status (PID + uptime, socket reachable, bind mount status, chroot patches applied), tail of `zd-spawnd.log`, and three actions: Restart daemon, Re-apply rootfs patches, Restore originals.
* Single-file HTML+CSS+JS, no build step. Uses the vanilla `ksu.exec` global so it runs unchanged on KernelSU, KSU WebUI Standalone for Magisk, and MMRL.
* "Re-apply rootfs patches" handles the case where `apt upgrade` inside the chroot overwrote `.bash_profile` / `.profile`. Previously only fixable by reinstalling the Magisk module.

### Refactor

* Chroot-patching logic moved from `customize.sh` into a standalone `chroot-init.sh`. `customize.sh` and the WebUI's "Re-apply" action both call it. Single source of truth, idempotent.

## v1.1.1 (2026-05-10)

Fixes for the chroot integrated terminal landing dir and `claude` / cargo / etc. on PATH.

### Fixes

* Integrated terminal now lands at the project cwd inside chroot instead of `/root`. NetHunter's `/root/.bash_profile` was unconditionally `cd`ing to `/root` and `cd ~` after sourcing dotfiles, wiping out the chdir we set pre-exec. `customize.sh` now patches `.bash_profile` so both `cd` lines are gated on `[ -z "$INIT_PWD" ]`. Zdroid sets `INIT_PWD` via the chroot adapter; non-Zdroid logins (e.g. `kali start` from a Termux shell) still land in `/root` as before.
* `~/.local/bin`, `~/.cargo/bin`, `~/.bun/bin`, `~/go/bin`, `~/.deno/bin`, `~/.npm-global/bin`, `~/.yarn/bin` are now prepended to PATH inside chroot. Stock `.profile` overwrote PATH with the canonical baseline only, silently dropping every user-installed tool. Replacement `.profile` snapshots `.bashrc`-injected PATH first (preserves nvm, pyenv, sdkman, asdf, etc.), then sets the canonical baseline, then merges the snapshot back, then prepends the user-bin dirs.
* Originals backed up to `.bash_profile.zdroid-orig` and `.profile.zdroid-orig` in the chroot. Magisk uninstall does not auto-restore them; if you remove the module and want the originals back, `mv` them in place yourself.

## v1.0.0 (2026-05-08)

Initial release.

### Highlights

* Persistent root-context spawn daemon (`zd-spawnd`, ~300 LOC C). One su elevation at boot via Magisk module; per-spawn cost ~5ms (Unix socket + fork+chroot+exec) vs. ~200ms via Magisk su mediation.
* Wire protocol with `SCM_RIGHTS` for stdio fd passing. See `PROTOCOL.md` next to the daemon source.
* Bind-mounts `/data/data/com.zdroid/files/home` onto `/zed` inside the chroot at startup so projects are reachable.
* `service.sh` supervisor restarts the daemon if it crashes, with a 5s backoff.
