# `futures::oneshot::Receiver::try_recv` semantics

**Status:** Active
**Phase / Commit:** L7
**Files:** `crates/gpui_android/src/multi_window.rs` (`create_extra_window_blocking`)

## Problem

We block the gpui game thread on a `futures::channel::oneshot::Receiver`
waiting for the JNI side to fire `surfaceCreated` and send the
`ANativeWindow` through. We can't park a future cleanly without an executor,
so the loop is poll-with-timeout:

```rust
loop {
    match rx.try_recv() {
        Ok(Some(value)) => return Ok(value),
        Ok(None) => bail!("channel dropped"),   // ← WRONG
        Err(_)   => { /* keep polling */ }      // ← WRONG
    }
    sleep(8.ms);
}
```

Symptom: `Open Settings` returns `Err("channel dropped")` immediately
on the first poll, even though the surface arrives ~500ms later. The Activity
launches anyway (fire-and-forget), but gpui never sees it; Settings is a
ghost window.

## Constraint

The intuitive semantics of `try_recv` for *most* channel types in Rust are
"either you get a value or the sender is gone." `futures::oneshot` is
different. From the actual API:

```rust
pub fn try_recv(&mut self) -> Result<Option<T>, Canceled>
```

- `Ok(Some(t))` — value received.
- `Ok(None)` — **value not yet sent, channel still open** (sender alive).
- `Err(Canceled)` — sender dropped without sending.

`Ok(None)` does NOT mean "channel dropped." The intuitive interpretation is
backwards.

## Solution

Map the three cases correctly. `Ok(None)` is the "still waiting" branch:

```rust
let deadline = std::time::Instant::now() + ACTIVITY_LAUNCH_TIMEOUT;
loop {
    match rx.try_recv() {
        Ok(Some(native_window)) => return Ok(native_window),
        Ok(None) => {
            // Not ready yet — fall through to deadline + sleep.
        }
        Err(_) => {
            pending_table().as_mut().and_then(|m| m.remove(&window_id));
            bail!("extra surface creation channel dropped (sender canceled)");
        }
    }
    if std::time::Instant::now() >= deadline {
        bail!("ExtraWindowActivity startup exceeded {}ms", ...);
    }
    std::thread::sleep(Duration::from_millis(8));
}
```

## Why this works

Lines up with the documented `try_recv` contract. The `Err(Canceled)` branch
is the only "real failure" — that's what we should bail on.

## Failure mode if regressed

- `Open Settings` always returns Err immediately regardless of whether the
  Activity actually launched.
- Activity continues launching in the background; gpui never registers it;
  user gets a chrome-only ghost window with no content.

## See also

- [Cold Activity launch timeout (4s)](activity-launch-cold-timeout.md) — how we pick the deadline
- [Multi-Activity OS-chromed extra windows](multi-activity-os-chrome.md)
