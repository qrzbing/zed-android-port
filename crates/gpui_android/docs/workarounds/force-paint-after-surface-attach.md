# Force a paint after surface attach

**Status:** Active
**Phase / Commit:** `61f43a0ac4` — Force a paint after surface attach

After ANativeWindow attaches (initial or after backgrounding), the swapchain has a fresh empty buffer but gpui's normal paint scheduling waits for an invalidation event. Result: blank window for one frame. Force an explicit paint after attach so the user sees content immediately.

**Detailed writeup: TODO** — full text TBD next time the area changes.
