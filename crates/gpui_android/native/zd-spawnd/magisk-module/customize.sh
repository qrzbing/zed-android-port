#!/sbin/sh
# Magisk install hook for the zdroid_spawnd module.
#
# Two responsibilities:
#   1. Set perms on $MODPATH so the daemon + service.sh can be executed.
#   2. Run chroot-init.sh to patch the user's chroot rootfs so a Zdroid-
#      spawned `bash -l` actually lands at the project cwd, instead of
#      NetHunter's hardcoded `cd /root` / `cd ~` in .bash_profile.
#
# The actual rootfs-patching logic lives in chroot-init.sh so the WebUI's
# "Re-apply patches" action can re-run it without going through a full
# Magisk reinstall (handy after `apt upgrade` inside the chroot).

ui_print "- Installing Zdroid Spawn Daemon"
ui_print "  Daemon will start on next boot (or magisk module reload)."
ui_print ""

# Daemon binary needs to be executable by root (magisk's exec context).
set_perm "$MODPATH/zd-spawnd" 0 0 0755

# Service script: executable, owner root.
set_perm "$MODPATH/service.sh" 0 0 0755

# Chroot init script: invoked from this customize.sh now, and from the
# WebUI later. Set explicit perms so it stays executable across reflashes.
set_perm "$MODPATH/chroot-init.sh" 0 0 0755

# Action script: presence is what makes Magisk Manager render the
# "Action" button on the module card (Magisk source: LocalModule.kt
# `hasAction = base.getChildFile("action.sh").exists()`). Magisk runs
# `sh action.sh` as root with cwd at the module dir; stdout pipes to
# its in-app console.
set_perm "$MODPATH/action.sh" 0 0 0755

# Uninstall hook: runs at next boot after Magisk Remove. Restores
# /root/.bash_profile and /root/.profile from .zdroid-orig backups
# inside the chroot, rmdirs the v1.1.6 symmetric bind-mount target
# dirs, removes the /data/user/0/com.zdroid alias symlink, and also
# cleans up the legacy v1.1.5- /zed target for upgrade-then-uninstall
# paths. Without this, "uninstall the module" leaves the chroot's
# bash startup permanently patched — surprising and wrong.
set_perm "$MODPATH/uninstall.sh" 0 0 0755

# Persistent log dir, OUTSIDE the module path so it survives future
# module updates. Magisk replaces /data/adb/modules/<id>/ wholesale on
# each install; logs there get wiped. Sibling at /data/adb/zdroid-spawnd/
# stays put across versions, which is the audit trail we want.
mkdir -p /data/adb/zdroid-spawnd/log
chmod 0750 /data/adb/zdroid-spawnd /data/adb/zdroid-spawnd/log
chown 0:0  /data/adb/zdroid-spawnd /data/adb/zdroid-spawnd/log

ui_print "- Files installed:"
ui_print "    $MODPATH/zd-spawnd       (daemon binary)"
ui_print "    $MODPATH/service.sh      (boot trigger; reactive uid + CE wait)"
ui_print "    $MODPATH/uninstall.sh    (Magisk Remove hook: restores rootfs)"
ui_print "    $MODPATH/chroot-init.sh  (rootfs patcher; also called from WebUI)"
ui_print "    $MODPATH/action.sh       (Magisk Action button: status snapshot)"
ui_print "    $MODPATH/webroot/        (WebUI: status, logs, interactive actions)"
ui_print "    $MODPATH/log/             (daemon logs)"
ui_print ""

# Patch the chroot. chroot-init.sh prints one status line per action;
# mirror them via ui_print so the install log shows the same detail the
# WebUI does later. Captures stderr too so failures surface here.
ui_print "- Running chroot-init.sh"
sh "$MODPATH/chroot-init.sh" 2>&1 | while IFS= read -r line; do
    ui_print "    $line"
done
ui_print ""

ui_print "- Reboot to start the daemon, OR run from Magisk:"
ui_print "    su -c $MODPATH/service.sh"
ui_print ""
ui_print "- Open the module's WebUI from KSU WebUI / MMRL to see daemon"
ui_print "  status, logs, and actions like Restart / Re-apply patches."
