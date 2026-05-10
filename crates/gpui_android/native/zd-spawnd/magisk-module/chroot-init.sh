#!/system/bin/sh
# Apply Zdroid's chroot rootfs init: patch /root/.bash_profile to honor
# $INIT_PWD, install /root/.profile with PATH-merge logic. Backs up the
# originals as <name>.zdroid-orig the first time the script runs.
#
# Idempotent: re-running re-applies the patches without clobbering the
# backups, which is what makes "Re-apply patches" useful in the WebUI
# after `apt upgrade` inside the chroot has overwritten our .bash_profile
# / .profile.
#
# Called from:
#   1. customize.sh during Magisk module install. ui_print decoration is
#      handled by customize.sh after this script runs.
#   2. webroot/index.html "Re-apply patches" action via:
#        ksu.exec(`sh /data/adb/modules/zdroid_spawnd/chroot-init.sh`)
#
# Usage:
#   sh chroot-init.sh [chroot_root]
#
# Args:
#   chroot_root  optional, defaults to /data/local/nhsystem/kali-arm64.
#                Must match `g_chroot_root` in zd-spawnd.c and the [chroot]
#                root field in zd-runtime.toml — Zdroid's spawn pipeline
#                hardcodes that path, so changing one without the other
#                breaks dispatch.
#
# Output:
#   Status lines on stdout, one per action. customize.sh mirrors them
#   via ui_print; WebUI displays them in a result panel.
#
# Exit codes:
#   0  success (or chroot dir not present, no-op)
#   1  patching failed mid-flight

set -e

CHROOT_ROOT="${1:-/data/local/nhsystem/kali-arm64}"

if [ ! -d "$CHROOT_ROOT/root" ]; then
    echo "chroot rootfs not found at $CHROOT_ROOT (skipping)"
    exit 0
fi

echo "patching chroot at $CHROOT_ROOT"

# .bash_profile: gate the two unconditional `cd` lines on $INIT_PWD so
# Zdroid-spawned bash login shells don't lose their cwd on startup.
BP="$CHROOT_ROOT/root/.bash_profile"
if [ -f "$BP" ] && [ ! -f "$BP.zdroid-orig" ]; then
    cp -p "$BP" "$BP.zdroid-orig"
    echo "  backed up .bash_profile -> .bash_profile.zdroid-orig"
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
echo "  .bash_profile patched (honors \$INIT_PWD)"

# .profile: replace stock NetHunter version with one that prepends user-
# bin dirs (claude, cargo, bun, go, deno, npm-global, yarn) AND merges
# any .bashrc-injected PATH (nvm, pyenv, sdkman, asdf, etc.) into the
# canonical baseline. See CHANGELOG v1.1.1 for the full rationale.
PR="$CHROOT_ROOT/root/.profile"
if [ -f "$PR" ] && [ ! -f "$PR.zdroid-orig" ]; then
    cp -p "$PR" "$PR.zdroid-orig"
    echo "  backed up .profile -> .profile.zdroid-orig"
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
echo "  .profile installed (prepends ~/.local/bin etc., merges .bashrc PATH)"

echo "done"
