#!/sbin/sh
# Magisk install hook for the zdroid_spawnd module.
#
# Magisk extracts the module zip into $MODPATH already; this script's
# job is just to set permissions and surface install-time UI text.
# Module files are auto-copied; service.sh runs at boot.

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
ui_print "- Reboot to start the daemon, OR run from Magisk:"
ui_print "    su -c $MODPATH/service.sh"
