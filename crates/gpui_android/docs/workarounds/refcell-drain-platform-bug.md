# platform.rs no-drain RefCell pattern

**Status:** Active

crates/gpui_android/src/platform.rs previously drained main_receiver while waiting for the native window to attach. That drain ran queued foreground runnables which call cx.update(...). open_window itself runs under cx.update, so the inner runnable hit 'RefCell already borrowed' and the app died. Fix: don't drain — let the outer event loop pick those runnables up on the next tick.

**Detailed writeup: TODO.** Stub created so the index links resolve.
