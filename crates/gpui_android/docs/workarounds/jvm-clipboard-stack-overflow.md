# JVM stack overflow on clipboard writes

**Status:** Active
**Phase / Commit:** `cbd61afd68` — Run Android clipboard writes on a worker thread
**Files:** `crates/gpui_android/src/clipboard.rs`

## Problem

`Cmd+C` from a Zed buffer or terminal sometimes crashed the app with a
JVM `StackOverflowError`. The crash was non-deterministic — small clipboard
payloads worked, larger ones didn't. The Rust-side `terminal::Copy` /
`editor::Copy` actions fired correctly, the JNI `write_inner` ran, the
crash happened mid-`printStackTrace` inside Android's `setPrimaryClip`
path.

## Constraint

Android's `android_main` thread has a **988 KB stack**. By the time gpui's
render-and-dispatch chain runs the JNI clipboard write, the Rust call stack
is already 70-90% deep. The remaining headroom can't accommodate
`setPrimaryClip` plus the Java side's lookup chain plus
`printStackTrace`'s recursive frame dumping.

This is the same class of bug as a read-path 60fps cascade we'd already
fixed earlier with caching: synchronous JNI work on the gpui thread is too
deep already to safely call back into the JVM for non-trivial work.

## Solution

Fire-and-forget clipboard writes on a dedicated thread with an explicit
2 MB stack:

```rust
let builder = std::thread::Builder::new()
    .name("zed-clipboard-write".into())
    .stack_size(2 * 1024 * 1024);
builder.spawn(move || {
    // Rebuild jni::JavaVM on the worker
    let vm = unsafe { JavaVM::from_raw(vm_ptr.cast())? };
    // ... do the actual setPrimaryClip JNI work
})?;
```

JNI primitives are extracted on the calling thread (`JavaVM*` and `Activity*`
are process-global), passed across the `std::thread` boundary as `usize`,
rebuilt on the worker.

A `WRITE_IN_FLIGHT` latch on the calling thread waits for the worker to
release it so we don't build up a queue of thread spawns under continuous
`Cmd+C` mashing.

## Why this works

- Worker thread starts with a fresh 2 MB stack — plenty of headroom for
  the full setPrimaryClip → ClipboardManager → ClipData chain.
- gpui thread frees up immediately after spawning, so the editor's input
  loop doesn't block on JVM I/O.
- The latch prevents thread-spawn DoS during repeated copy.

## Failure mode if regressed

- Synchronous JNI write reintroduced → app crashes on copy of any payload
  large enough to push past the remaining headroom. Crash is `StackOverflowError`
  in Java side, not a clean Rust panic, so it's harder to spot in logcat.
- Worker stack reduced too far → same bug with a different threshold.
- Latch removed → spammed copy ops spawn unbounded threads, OOM the
  process.

## See also

- [refcell-drain-platform-bug.md](refcell-drain-platform-bug.md) — same
  thread / stack class of issue around open_window
- [activity-recreation-idempotency.md](activity-recreation-idempotency.md)
  — JNI primitives we can safely cache across activity boundaries
