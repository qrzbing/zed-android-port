# Choreographer-driven vsync

**Status:** Active

crates/gpui_android/src/platform.rs uses AChoreographer FFI to drive frame callbacks. Replaces the previous 8ms fixed-interval ALooper poll with event-driven vsync alignment to the device's actual display refresh. Side effect: 'Spurious ALOOPER_POLL_CALLBACK' log spam every ~33ms; non-fatal.

**Detailed writeup: TODO.** Stub created so the index links resolve.
