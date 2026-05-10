#!/system/bin/sh
# Magisk module-removal hook. Runs at the next boot AFTER the user
# marks the module for removal in Magisk Manager (or via `magisk
# --remove-modules`), BEFORE Magisk deletes /data/adb/modules/<id>.
#
# Why this exists: customize.sh writes patched .bash_profile and
# .profile into the chroot rootfs at install time, with the originals
# saved as <name>.zdroid-orig. If we leave it at "module dir gone",
# the user's chroot still has Zdroid-flavored bash startup forever —
# they uninstalled the module specifically to revert, and the revert
# didn't happen. This script restores the originals.
#
# Touches only files under $CHROOT_ROOT, which is in DE space (not
# user-encrypted), so we don't need an FBE / CE-unlock wait here.
#
# Output goes to a log file outside the module dir so the user can
# audit what was reverted post-uninstall. Module dir gets deleted
# right after this script returns.

CHROOT_ROOT="/data/local/nhsystem/kali-arm64"
LOG="/data/adb/zdroid-spawnd-uninstall.log"

log() {
    echo "$(date -Iseconds) [uninstall] $*" >> "$LOG"
}

log "starting module uninstall cleanup"

if [ ! -d "$CHROOT_ROOT/root" ]; then
    log "chroot rootfs not present at $CHROOT_ROOT; nothing to revert"
    exit 0
fi

# Restore .bash_profile if we have a backup.
BP="$CHROOT_ROOT/root/.bash_profile"
if [ -f "$BP.zdroid-orig" ]; then
    if mv "$BP.zdroid-orig" "$BP"; then
        log "restored .bash_profile from .zdroid-orig"
    else
        log "WARN: failed to restore .bash_profile (mv returned $?)"
    fi
else
    log ".bash_profile.zdroid-orig missing; nothing to restore"
fi

# Restore .profile if we have a backup.
PR="$CHROOT_ROOT/root/.profile"
if [ -f "$PR.zdroid-orig" ]; then
    if mv "$PR.zdroid-orig" "$PR"; then
        log "restored .profile from .zdroid-orig"
    else
        log "WARN: failed to restore .profile (mv returned $?)"
    fi
else
    log ".profile.zdroid-orig missing; nothing to restore"
fi

# Remove the bind-mount target dir if it's empty. The bind itself died
# with the daemon — at this boot the daemon hasn't run (module being
# removed). If the dir is empty, it's just an mkdir we did at install
# and rmdir leaves the chroot tidy. If something else is there
# (shouldn't be), rmdir fails and we leave it for the user.
ZED_DIR="$CHROOT_ROOT/zed"
if [ -d "$ZED_DIR" ]; then
    if rmdir "$ZED_DIR" 2>/dev/null; then
        log "removed empty bind-mount target $ZED_DIR"
    else
        log "$ZED_DIR is non-empty; left in place"
    fi
fi

log "cleanup done"
