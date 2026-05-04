# JVM stack overflow on clipboard

**Status:** Active

Android's android_main thread is 988 KB; synchronous clipboard JNI from Rust overflows it on bigger payloads. Fix: clipboard writes happen on a worker thread (background_spawn) instead of the main thread. crates/gpui_android/src/clipboard.rs handles the dispatch.

**Detailed writeup: TODO.** Stub created so the index links resolve.
