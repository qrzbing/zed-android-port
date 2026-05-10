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
#
# Race we hit in v1.1.2: sys.boot_completed=1 fires when system_server
# is up, but PackageManager's per-user app data sometimes isn't readable
# yet (encrypted-storage decryption + first-run /data/data/<pkg> setup
# can lag). A single stat returns empty, the script bails, daemon never
# starts, user has to manually run service.sh after login. Real users
# don't know to do that.
#
# Fix: poll for up to 60s. stat returning 0 (root, before app reinstalls
# fix ownership) also counts as "not ready". Most boots resolve within
# 1-3s of this loop.
uid_wait_iters=0
ZDROID_UID=""
while [ "$uid_wait_iters" -lt 60 ]; do
    ZDROID_UID=$(stat -c %u /data/data/com.zdroid 2>/dev/null)
    if [ -n "$ZDROID_UID" ] && [ "$ZDROID_UID" != "0" ]; then
        break
    fi
    sleep 1
    uid_wait_iters=$((uid_wait_iters + 1))
done

if [ -z "$ZDROID_UID" ] || [ "$ZDROID_UID" = "0" ]; then
    log "com.zdroid uid did not resolve within 60s; not starting daemon"
    log "  (app may not be installed; run 'sh $MODDIR/service.sh' once installed)"
    exit 0
fi
log "resolved zdroid uid: $ZDROID_UID (after ${uid_wait_iters}s)"

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
