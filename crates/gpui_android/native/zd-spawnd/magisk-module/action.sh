#!/system/bin/sh
# Magisk Manager "Action" button entry point.
#
# Magisk renders the Action button on a module card if and only if
# /data/adb/modules/<id>/action.sh exists (see Magisk source:
# core/.../LocalModule.kt `hasAction`). When the user taps it, Magisk
# runs `sh action.sh` as root with cwd at the module dir, captures
# stdout, and shows it in an in-app console (legacy UI) or a built-in
# terminal (apk-ng UI). No intent dispatch, no WebView.
#
# This script's job is to print a useful one-shot status snapshot so
# the user can answer "is it healthy" without leaving Magisk Manager:
# daemon PID + uptime, socket reachable, bind mount status, chroot
# patches applied, last log lines.
#
# For interactive control (restart daemon, re-apply patches, restore
# originals), point at the WebUI: KSU WebUI Standalone, MMRL, or any
# viewer that loads webroot/index.html.

CHROOT_ROOT="/data/local/nhsystem/kali-arm64"
MODDIR=${0%/*}
SOCKET="/data/data/com.zdroid/files/run/zd-spawn"

echo "Zdroid Spawn Daemon — status"
echo "============================="
echo

# Daemon
PID=$(pgrep -x zd-spawnd | head -1)
if [ -n "$PID" ]; then
    INFO=$(ps -o pid,etime,comm -p "$PID" | tail -n +2 | tr -s ' ')
    echo "[OK]   Daemon running: $INFO"
else
    echo "[ERR]  Daemon NOT running"
    echo "       Start manually: su -c $MODDIR/service.sh"
fi

# Socket
if [ -S "$SOCKET" ]; then
    echo "[OK]   Socket reachable at $SOCKET"
else
    echo "[ERR]  Socket missing at $SOCKET"
fi

# Bind mount
if mount | grep -q "$CHROOT_ROOT/zed"; then
    SRC=$(mount | grep "$CHROOT_ROOT/zed" | head -1 | awk '{print $1}')
    echo "[OK]   /zed bind active (backing: $SRC)"
else
    echo "[WARN] /zed bind NOT active"
    echo "       Daemon establishes this at startup. Check service log."
fi

# Chroot patches
if [ -f "$CHROOT_ROOT/root/.bash_profile.zdroid-orig" ] && \
   [ -f "$CHROOT_ROOT/root/.profile.zdroid-orig" ]; then
    echo "[OK]   Chroot patches applied (.zdroid-orig backups present)"
else
    echo "[WARN] Chroot patches NOT applied"
    echo "       Re-run: sh $MODDIR/chroot-init.sh"
fi

# Zdroid app uid (sanity check that PackageManager sees the app)
ZUID=$(stat -c %u /data/data/com.zdroid 2>/dev/null)
if [ -n "$ZUID" ] && [ "$ZUID" != "0" ]; then
    echo "[OK]   Zdroid app uid: $ZUID"
else
    echo "[WARN] Zdroid app (com.zdroid) not installed or unreachable"
fi

echo
echo "Last 10 log lines"
echo "-----------------"
tail -n 10 "$MODDIR/log/zd-spawnd.log" 2>/dev/null || echo "(no log yet)"

echo
echo "For interactive controls (restart, re-apply patches, restore"
echo "originals), open the WebUI in KSU WebUI Standalone or MMRL."
