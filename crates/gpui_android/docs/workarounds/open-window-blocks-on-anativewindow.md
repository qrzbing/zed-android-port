# Block open_window until ANativeWindow is ready

**Status:** Active
**Phase / Commit:** `d68a77afcc` — Block open_window until ANativeWindow ready

AndroidPlatform::open_window blocks the Rust caller until the JVM-side ANativeWindow has actually attached. Without this, the renderer races against surface creation — first paint goes to a null surface, wgpu errors, app shows a black frame for 100-300ms before recovering. The block waits on a condvar that the surface-attach event releases.

**Detailed writeup: TODO** — full text TBD next time the area changes.
