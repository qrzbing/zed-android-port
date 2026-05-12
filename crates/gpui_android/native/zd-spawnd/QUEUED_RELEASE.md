# Queued for next zdroid-spawnd release

Working file for changes that are NOT yet shipped. Lives next to the
module source, not inside `magisk-module/` (this file doesn't go into
the release zip). When the queue accumulates to something worth a
re-install, we batch it into a single version bump.

**Cadence rule**: don't ship a release for a single cosmetic / one-fix
change. Iterate locally with `adb push`, test on device, only cut a
new release tag + GitHub asset when the changeset is something a
downstream user would meaningfully want to re-install for.

The current shipped version is in [magisk-module/module.prop](magisk-module/module.prop).

## Open items

### Diagnostics

- [ ] **rc encoding disambiguation in daemon log.** Currently
      `exit_code = -WTERMSIG(status)` aliases SIGHUP (signum 1) with
      our else-fallback (-1 for unknown wait status). Verified on
      device: SIGTERM produces `rc=-15`, SIGHUP-from-PTY-close produces
      `rc=-1`, but unknown status would also produce `rc=-1`. Indistinguishable.
      Fix: log as `exit=normal rc=0` / `exit=signal SIGHUP duration=…`
      / `exit=unknown status=0x???? duration=…`. Cosmetic but makes the
      log honest.
      File: `zd-spawnd.c::handle_connection`.

### WebUI

- [ ] **Performance card in sidebar.** Surface the per-spawn timing
      claim. Place between Status and Info. Pulls from the same
      batched status `ksu.exec` (no extra su grant) via
      `grep -oE 'fork=[0-9]+ms' .../zd-spawnd.log | tail -100`,
      reduces to mean / max / count. Three rows:
      `Spawn count: N / Fork latency mean: Xms / max: Yms`.

- [ ] **Status row formatting**: Daemon row currently shows
      `pid 2194 (4)` because `ps -o etime` on a freshly-started process
      returns just `4` (4 seconds), and our JS just splits on space and
      takes index [1]. Format explicitly: `pid 2194 (uptime 4s)` /
      `(uptime 1h12m)` / etc. Parsing logic in
      `webroot/index.html::refreshStatus`.

- [ ] **Bind dst value wrapping.** In the Info card the
      `/data/local/nhsystem/kali-arm64/zed` value wraps to two lines on
      narrower sidebar widths. Either narrow the value column with
      ellipsis + tooltip, or shorten the label space, or accept it.

### Daemon

- [ ] **Kill-protocol implementation.** `chroot.rs::ChrootSpawnHandle::kill`
      shuts down the write half of the socket and the comment claims
      "Daemon reads 0, SIGKILLs the child, sends response_exited with -9."
      Daemon does not actually do that. It blocks in `waitpid` until the
      child exits naturally; `shutdown(WR)` from client is invisible.
      Either wire the daemon to react (epoll on the socket alongside
      waitpid; on read EOF, SIGKILL the child), or update the comment in
      chroot.rs to reflect reality and document the actual kill mechanism
      (PTY close + SIGHUP for interactive spawns, depends on parent for
      others).

- [ ] **Log-write failure detection.** `fwrite` return is unchecked
      today. Disk full / fs error → silent log loss. Check return,
      flag a "log writes failing" state surfaced in action.sh + WebUI.

### Service

- [ ] **Stats line every N spawns.** Periodic summary in the log:
      `INFO stats: 100 spawns since boot, fork mean=Xms max=Yms`.
      Cheap (running counters in daemon) and gives a "what's been
      happening" glance without parsing every spawn line.

## Recently shipped (kept here briefly for reference)

- 2026-05-12, APK-side (no spawnd version bump): runtime adapter
  abstraction extended with `RuntimeProvider::environment_root`. Each
  adapter (chroot/bootstrap/external-Termux) owns its own env-aware
  data root: `languages/`, `extensions/`, `debug_adapters/`,
  `copilot/`, `prettier/`, `remote_servers/`, `devcontainer/`,
  `external_agents/`, `remote_extensions/`. Zed's `paths::*` env-aware
  dirs derive from a new `environment_root` setter; wiring happens
  once at boot. Chroot adapter argv translation rewrites host paths
  embedded in spawn arguments to their chroot-target equivalents so
  Zed-installed LSPs resolve cleanly inside chroot.
- v1.1.5 (2026-05-10): WebUI Material 3 redesign + 2-col layout, log
  moved to `/data/adb/zdroid-spawnd/log/`, log rotation, per-spawn
  timing instrumentation, action button bridges to KSU WebUI via
  intent.
- v1.1.4: reactive boot signals (sys.boot_completed + pm + ce_available),
  uninstall.sh.
- v1.1.3: action.sh, batched WebUI status, service.sh boot retry
  (later replaced by reactive signals in v1.1.4).
- v1.1.2: WebUI panel, chroot-init.sh refactor.
- v1.1.1: chroot patches via customize.sh, INIT_PWD env, HOME=/root.
- v1.1.0: initial daemon + Magisk module packaging.

When the queue above gets cut into a release, move the items to this
"Recently shipped" section under the new version heading and prune
older entries (CHANGELOG.md inside the module is the long-term record).
