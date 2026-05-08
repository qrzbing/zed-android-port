#!/system/bin/sh
# Magisk late-start service for zdroid_spawnd.
#
# Magisk runs this at the late_start_service trigger — after /data is
# decrypted, after Android boot is mostly complete. Inherits root.
#
# We supervise the daemon: if it crashes, restart with a small backoff
# so a runaway crash doesn't pin a CPU.

MODDIR=${0%/*}
LOG="$MODDIR/log/zd-spawnd.log"

mkdir -p "$MODDIR/log"

log() {
    echo "$(date -Iseconds) [service] $*" >> "$LOG"
}

# Wait until Android boot completes — `stat /data/data/<pkg>` only
# returns a meaningful uid after PackageManager is up. Bail after 120s
# if boot is somehow stuck so we don't pin a process forever.
boot_wait_iters=0
until [ "$(getprop sys.boot_completed 2>/dev/null)" = "1" ]; do
    sleep 1
    boot_wait_iters=$((boot_wait_iters + 1))
    if [ "$boot_wait_iters" -ge 120 ]; then
        log "boot timeout (>120s); aborting service start"
        exit 0
    fi
done

# Resolve Zdroid's app uid from PackageManager. The daemon needs this
# to set the socket's group ownership and verify peer credentials.
ZDROID_UID=$(stat -c %u /data/data/com.zdroid 2>/dev/null)
if [ -z "$ZDROID_UID" ] || [ "$ZDROID_UID" = "0" ]; then
    log "com.zdroid not installed (uid empty/root); not starting daemon"
    exit 0
fi
log "resolved zdroid uid: $ZDROID_UID"

# Supervise loop. `exec` would replace the service.sh process; we use a
# subshell so service.sh exits and Magisk's service tracker knows the
# trigger ran. The supervisor backgrounds itself with nohup.
nohup sh -c '
    LOG="'"$LOG"'"
    DAEMON="'"$MODDIR"'/zd-spawnd"
    UID_ARG="'"$ZDROID_UID"'"
    while true; do
        echo "$(date -Iseconds) [supervisor] starting daemon" >> "$LOG"
        "$DAEMON" "$UID_ARG" >> "$LOG" 2>&1
        rc=$?
        echo "$(date -Iseconds) [supervisor] daemon exited rc=$rc, restarting in 5s" >> "$LOG"
        sleep 5
    done
' >/dev/null 2>&1 &

log "supervisor started (pid $!)"
