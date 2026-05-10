#!/system/bin/sh
# Magisk Action button entry point.
#
# Magisk renders the Action button on a module card if and only if
# /data/adb/modules/<id>/action.sh exists, then runs `sh action.sh`
# as root with cwd at the module dir and pipes stdout to its in-app
# console.
#
# Primary behavior: bridge to KSU WebUI Standalone via Android intent
# so the user lands directly in the interactive panel for THIS module
# (not KSU WebUI's module list — they tapped Action on Zdroid Spawn
# Daemon, take them to Zdroid Spawn Daemon's WebUI). Fall back to a
# textual status snapshot when the bridge fails (KSU WebUI not
# installed, am rejection, etc.) so the button is never useless.
#
# Bridge details: KSU WebUI Standalone's WebUIActivity is exported
# without an intent-filter, so we specify the component explicitly
# and pass the module id + display name as extras. Source:
#   https://github.com/5ec1cff/KsuWebUIStandalone
#   app/src/main/java/io/github/a13e300/ksuwebui/WebUIActivity.kt
# expects:
#   intent.getStringExtra("id")   — module id, e.g. zdroid_spawnd
#   intent.getStringExtra("name") — display label for task description

WEBUI_PKG="io.github.a13e300.ksuwebui"
WEBUI_ACTIVITY="$WEBUI_PKG/.WebUIActivity"
MODULE_ID="zdroid_spawnd"
MODULE_NAME="Zdroid Spawn Daemon"

CHROOT_ROOT="/data/local/nhsystem/kali-arm64"
LOG="/data/adb/zdroid-spawnd/log/zd-spawnd.log"
SOCKET="/data/data/com.zdroid/files/run/zd-spawn"

webui_installed() {
    pm list packages "$WEBUI_PKG" 2>/dev/null | grep -q "^package:$WEBUI_PKG\$"
}

open_webui() {
    am start -n "$WEBUI_ACTIVITY" \
        --es id "$MODULE_ID" \
        --es name "$MODULE_NAME" \
        >/dev/null 2>&1
}

if webui_installed; then
    if open_webui; then
        echo "Opening Zdroid Spawn Daemon WebUI in KSU WebUI Standalone."
        echo "If the activity didn't come up, check Magisk's su grant for"
        echo "io.github.a13e300.ksuwebui."
        exit 0
    fi
    echo "KSU WebUI Standalone is installed but the launch intent"
    echo "failed (am rejected the start). Falling back to status."
    echo
fi

# WebUI bridge unavailable — print the status snapshot directly so the
# Action button is still useful for "is the daemon healthy" checks.
if ! webui_installed; then
    echo "KSU WebUI Standalone is not installed."
    echo "Install for one-tap interactive controls (restart daemon,"
    echo "re-apply patches, restore originals):"
    echo "  https://github.com/5ec1cff/KsuWebUIStandalone"
    echo
fi

echo "Zdroid Spawn Daemon — status"
echo "============================="
echo

PID=$(pgrep -x zd-spawnd | head -1)
if [ -n "$PID" ]; then
    INFO=$(ps -o pid,etime,comm -p "$PID" | tail -n +2 | tr -s ' ')
    echo "[OK]   Daemon running: $INFO"
else
    echo "[ERR]  Daemon NOT running"
    echo "       Start manually: su -c /data/adb/modules/zdroid_spawnd/service.sh"
fi

if [ -S "$SOCKET" ]; then
    echo "[OK]   Socket reachable at $SOCKET"
else
    echo "[ERR]  Socket missing at $SOCKET"
fi

if mount | grep -q "$CHROOT_ROOT/zed"; then
    SRC=$(mount | grep "$CHROOT_ROOT/zed" | head -1 | awk '{print $1}')
    echo "[OK]   /zed bind active (backing: $SRC)"
else
    echo "[WARN] /zed bind NOT active"
    echo "       Daemon establishes this at startup. Check service log."
fi

if [ -f "$CHROOT_ROOT/root/.bash_profile.zdroid-orig" ] && \
   [ -f "$CHROOT_ROOT/root/.profile.zdroid-orig" ]; then
    echo "[OK]   Chroot patches applied (.zdroid-orig backups present)"
else
    echo "[WARN] Chroot patches NOT applied"
    echo "       Re-run: sh /data/adb/modules/zdroid_spawnd/chroot-init.sh"
fi

ZUID=$(stat -c %u /data/data/com.zdroid 2>/dev/null)
if [ -n "$ZUID" ] && [ "$ZUID" != "0" ]; then
    echo "[OK]   Zdroid app uid: $ZUID"
else
    echo "[WARN] Zdroid app (com.zdroid) not installed or unreachable"
fi

echo
echo "Last 10 log lines"
echo "-----------------"
tail -n 10 "$LOG" 2>/dev/null || echo "(no log yet)"
