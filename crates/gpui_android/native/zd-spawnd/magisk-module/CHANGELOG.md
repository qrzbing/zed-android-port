# Zdroid Spawn Daemon changelog

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
