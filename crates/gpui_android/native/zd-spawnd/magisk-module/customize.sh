#!/sbin/sh
# Magisk install hook for the zdroid_spawnd module.
#
# Two responsibilities:
#   1. Set perms on $MODPATH so the daemon + service.sh can be executed.
#   2. Patch the user's chroot rootfs at $CHROOT_ROOT so a Zdroid-spawned
#      `bash -l` actually lands at the project cwd we asked for, instead
#      of NetHunter's hardcoded `cd /root` / `cd ~` in .bash_profile.

ui_print "- Installing Zdroid Spawn Daemon"
ui_print "  Daemon will start on next boot (or magisk module reload)."
ui_print ""

# Daemon binary needs to be executable by root (magisk's exec context).
set_perm "$MODPATH/zd-spawnd" 0 0 0755

# Service script: executable, owner root.
set_perm "$MODPATH/service.sh" 0 0 0755

# Logs dir, world-unreadable; daemon writes here.
mkdir -p "$MODPATH/log"
set_perm "$MODPATH/log" 0 0 0750

ui_print "- Files installed:"
ui_print "    $MODPATH/zd-spawnd       (daemon binary)"
ui_print "    $MODPATH/service.sh      (boot trigger)"
ui_print "    $MODPATH/log/             (daemon logs)"
ui_print ""

# ===== Chroot rootfs init =================================================
#
# Patches /root/.bash_profile and /root/.profile inside the kali-arm64
# rootfs so that:
#
#   - .bash_profile honors $INIT_PWD (set by Zdroid's chroot adapter to
#     the translated project cwd). NetHunter's stock .bash_profile runs
#     `cd /root` then `cd ~` unconditionally, wiping out the cwd that
#     zd-spawnd's chdir(2) set up before exec'ing bash. Gating those two
#     `cd`s on `[ -z "$INIT_PWD" ]` preserves NetHunter's `kali start`
#     UX (no INIT_PWD → land in /root) while letting Zdroid pin a
#     specific landing dir.
#
#   - .profile prepends the standard user-bin dirs ($HOME/.local/bin,
#     ~/.cargo/bin, etc.) to PATH. Without this, npm / pip --user /
#     `claude` installs aren't on PATH inside the chroot's interactive
#     bash. NetHunter ships a .profile that hard-overwrites PATH with
#     no user-bin prepend; we replace it with one that does both the
#     standard PATH and the prepend.
#
# Both files are backed up to <name>.zdroid-orig the first time so the
# user can restore the upstream NetHunter version with a single mv.
# Idempotent: re-installing the module re-applies the patches without
# clobbering existing backups.
#
# CHROOT_ROOT must match `g_chroot_root` in zd-spawnd.c and the `root`
# field of [chroot] in zd-runtime.toml. If you change it, change all
# three.

CHROOT_ROOT="/data/local/nhsystem/kali-arm64"

if [ ! -d "$CHROOT_ROOT/root" ]; then
    ui_print "- Chroot rootfs not found at $CHROOT_ROOT"
    ui_print "  Skipping rootfs patch. Re-flash this module after"
    ui_print "  installing kali-arm64 to apply the bash startup fixes."
    ui_print ""
else
    ui_print "- Patching chroot at $CHROOT_ROOT"

    BP="$CHROOT_ROOT/root/.bash_profile"
    if [ -f "$BP" ] && [ ! -f "$BP.zdroid-orig" ]; then
        cp -p "$BP" "$BP.zdroid-orig"
        ui_print "    backed up .bash_profile -> .bash_profile.zdroid-orig"
    fi
    cat > "$BP" <<'BASH_PROFILE_EOF'
# Patched by zdroid-spawnd Magisk module. Original at .bash_profile.zdroid-orig.
#
# The two `cd` lines below are gated on INIT_PWD so a Zdroid-spawned
# bash (which sets INIT_PWD to the project cwd via chroot.rs) does not
# get its working dir overwritten on login. Non-Zdroid logins (e.g.
# `kali start` from a Termux shell) leave INIT_PWD unset and land in
# /root as before.

export TERM=xterm-256color
stty columns 80
[ -z "$INIT_PWD" ] && cd /root

if [ ! -d /dev/net ]; then
  mkdir -pv /dev/net
  ln -sfv /dev/tun /dev/net/tun
fi

if [ ! -d /dev/fd ]; then
  ln -sfv /proc/self/fd /dev/fd
  ln -sfv /dev/fd/0 /dev/stdin
  ln -sfv /dev/fd/1 /dev/stdout
  ln -sfv /dev/fd/2 /dev/stderr
fi

. /root/.bashrc
. /root/.profile

[ -z "$INIT_PWD" ] && cd ~
BASH_PROFILE_EOF
    chmod 0644 "$BP"
    chown 0:0 "$BP"
    ui_print "    .bash_profile patched (honors \$INIT_PWD)"

    PR="$CHROOT_ROOT/root/.profile"
    if [ -f "$PR" ] && [ ! -f "$PR.zdroid-orig" ]; then
        cp -p "$PR" "$PR.zdroid-orig"
        ui_print "    backed up .profile -> .profile.zdroid-orig"
    fi
    cat > "$PR" <<'PROFILE_EOF'
# Patched by zdroid-spawnd Magisk module. Original at .profile.zdroid-orig.

if [ "$BASH" ]; then
  if [ -f ~/.bashrc ]; then
    . ~/.bashrc
  fi
fi

# Snapshot whatever .bashrc has added to PATH (nvm / pyenv / rbenv /
# sdkman / asdf / yarn / pnpm / custom user exports — anything that
# wires itself in via .bashrc). We're about to overwrite PATH with the
# NetHunter-canonical baseline; snapshot first so we can merge those
# additions back in afterwards. Without this merge, version managers
# silently lose their shims on every fresh login.
PRE_BASHRC_PATH="$PATH"

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/system/xbin:/system/bin"

# Merge: re-add anything from PRE_BASHRC_PATH that isn't already in the
# canonical PATH. Appended (not prepended) so canonical kali/system dirs
# keep priority over .bashrc-injected ones; user-bin dirs we explicitly
# care about get prepended in the next loop.
OLD_IFS="$IFS"
IFS=':'
for d in $PRE_BASHRC_PATH; do
  [ -z "$d" ] && continue
  case ":$PATH:" in
    *":$d:"*) ;;
    *) PATH="$PATH:$d" ;;
  esac
done
IFS="$OLD_IFS"
unset PRE_BASHRC_PATH

# Prepend known user-bin dirs (these win over system locations).
# Idempotent: re-sourcing does not duplicate entries.
#   ~/.local/bin     pip --user, pipx, Claude Code, most modern installers
#   ~/bin            POSIX convention
#   ~/.cargo/bin     Rust
#   ~/.bun/bin       Bun
#   ~/go/bin         Go (`go install` default)
#   ~/.deno/bin      Deno
#   ~/.npm-global/bin  npm with `npm config set prefix ~/.npm-global`
#   ~/.yarn/bin      Yarn classic global
for d in "$HOME/.local/bin" "$HOME/bin" "$HOME/.cargo/bin" "$HOME/.bun/bin" "$HOME/go/bin" "$HOME/.deno/bin" "$HOME/.npm-global/bin" "$HOME/.yarn/bin"; do
  if [ -d "$d" ]; then
    case ":$PATH:" in
      *":$d:"*) ;;
      *) PATH="$d:$PATH" ;;
    esac
  fi
done
export PATH

export TMPDIR=/tmp
PROFILE_EOF
    chmod 0644 "$PR"
    chown 0:0 "$PR"
    ui_print "    .profile installed (prepends ~/.local/bin etc.)"
    ui_print ""
fi

ui_print "- Reboot to start the daemon, OR run from Magisk:"
ui_print "    su -c $MODPATH/service.sh"
