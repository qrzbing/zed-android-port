# Activity-recreation idempotency

**Status:** Active

android_main can re-enter on activity recreation. Our boot does set_var, paths::set_custom_data_dir, mkdir, etc. — all wrapped in OnceLock guards or content-compare gates so the second invocation is a no-op. set_var is sound across re-entry because the values are deterministic and JVM service threads don't touch libc env.

**Detailed writeup: TODO.** Stub created so the index links resolve.
