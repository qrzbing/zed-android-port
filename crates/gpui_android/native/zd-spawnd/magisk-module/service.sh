#!/system/bin/sh
# Magisk late-start service for zdroid_spawnd.
#
# Magisk runs this at the late_start_service trigger — after /data is
# decrypted, after Android boot is mostly complete. Inherits root.
#
# We supervise the daemon: if it crashes, restart with a small backoff
# so a runaway crash doesn't pin a CPU.
#
# Three readiness gates before we start the daemon. Each watches the
# canonical AOSP-set signal for its precondition; no blind retry on
# the operation itself. Each `getprop` call is microseconds against
# Android's shared-memory property service, so the polls are cheap
# even at sub-second granularity:
#
#   1. sys.boot_completed=1     — system_server is alive (PackageManager
#                                  reachable, init triggers fired).
#   2. pm list -U returns a uid — Zdroid is installed. Lookup goes
#                                  through PackageManager (DE space),
#                                  not /data/data (CE-encrypted), so it
#                                  works pre-unlock.
#   3. sys.user.0.ce_available  — User 0's credential-encrypted storage
#                                  is unlocked. AOSP-set by `vold` in
#                                  OnUserUnlocked. After this flips,
#                                  /data/data/com.zdroid/files/run/ is
#                                  creatable so the daemon can bind its
#                                  socket there.
#
# Order matters: we resolve the uid (which doesn't need CE) before
# waiting for unlock so a "Zdroid never installed" case fails fast on
# `pm list -U returning empty`, instead of waiting forever for an
# unlock event for an app that doesn't exist.

MODDIR=${0%/*}
LOG="$MODDIR/log/zd-spawnd.log"

mkdir -p "$MODDIR/log"

log() {
    echo "$(date -Iseconds) [service] $*" >> "$LOG"
}

# Gate 1: sys.boot_completed=1. Bail after 120s — boot stuck means
# something else is broken; pinning a script forever doesn't help.
boot_wait_iters=0
until [ "$(getprop sys.boot_completed 2>/dev/null)" = "1" ]; do
    sleep 1
    boot_wait_iters=$((boot_wait_iters + 1))
    if [ "$boot_wait_iters" -ge 120 ]; then
        log "boot timeout (>120s); aborting"
        exit 0
    fi
done
log "sys.boot_completed=1 (after ${boot_wait_iters}s)"

# Gate 2: Zdroid installed, uid resolvable via PackageManager.
# `pm list packages -U <pkg>` output: `package:<pkg> uid:<n>,<n>...`
# The first uid is user 0's primary; downstream uids belong to managed
# users / work profiles we don't care about for the socket peer-cred
# check. Empty output means PM doesn't know the package — wait quietly.
ZDROID_UID=""
pm_wait_iters=0
while [ -z "$ZDROID_UID" ]; do
    ZDROID_UID=$(pm list packages -U com.zdroid 2>/dev/null \
                 | sed -nE 's/.*uid:([0-9]+).*/\1/p' | head -1)
    [ -n "$ZDROID_UID" ] && break
    sleep 2
    pm_wait_iters=$((pm_wait_iters + 1))
    # Log every 30 iterations (= 1 minute) so a long wait stays
    # diagnosable without flooding the log.
    if [ $((pm_wait_iters % 30)) -eq 0 ]; then
        log "pm list -U com.zdroid still empty (waited $((pm_wait_iters * 2))s; install Zdroid to start daemon)"
    fi
done
log "resolved zdroid uid: $ZDROID_UID via pm (after $((pm_wait_iters * 2))s)"

# Gate 3: user 0 CE storage unlocked. AOSP-universal: written by
# vold's OnUserUnlocked. We need this because the daemon binds its
# socket under /data/data/com.zdroid/files/run/, which is CE-encrypted
# and unreadable / unwritable until the user unlocks once after boot.
ce_wait_iters=0
until [ "$(getprop sys.user.0.ce_available 2>/dev/null)" = "true" ]; do
    sleep 1
    ce_wait_iters=$((ce_wait_iters + 1))
    if [ $((ce_wait_iters % 60)) -eq 0 ]; then
        log "sys.user.0.ce_available not yet true (waited ${ce_wait_iters}s; user has not unlocked)"
    fi
done
log "user 0 CE storage unlocked (after ${ce_wait_iters}s)"

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
